// Copyright 2026 Chris Olson
// Portions derived from ADK-Go (https://github.com/google/adk-go), Copyright 2026 Google LLC.
// Portions derived from google-api-go-client (https://github.com/googleapis/google-api-go-client),
// Copyright 2020 Google LLC.
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

//! Google ID token verification for [`google_oidc`](super::google_oidc): a port of
//! `google.golang.org/api/idtoken` `Validate` (v0.304.0 `validate.go`, `cache.go`). RS256 tokens
//! verify against Google's `oauth2/v3/certs` key set, ES256 tokens against the IAP key set; each
//! set is cached per URL until its `Cache-Control: max-age` (less `Age`) runs out. Signatures are
//! checked with `jsonwebtoken` (aws-lc-rs); the steps, their order and the error texts are
//! idtoken's.

use std::collections::HashMap;
use std::sync::{Arc, LazyLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use base64::Engine as _;
use base64::alphabet;
use base64::engine::{DecodePaddingMode, GeneralPurpose, GeneralPurposeConfig};
use futures::FutureExt as _;
use futures::future::{BoxFuture, Shared};
use gadk_platform::go_quote;
use jsonwebtoken::{Algorithm, DecodingKey};
use parking_lot::Mutex;
use serde::Deserialize;
use serde_json::{Map, Value};

/// idtoken `googleSACertsURL`: the key set of Google-signed (RS256) ID tokens.
pub(crate) const GOOGLE_SA_CERTS_URL: &str = "https://www.googleapis.com/oauth2/v3/certs";
/// idtoken `googleIAPCertsURL`: the key set of IAP-signed (ES256) ID tokens.
pub(crate) const GOOGLE_IAP_CERTS_URL: &str = "https://www.gstatic.com/iap/verify/public_key-jwk";

/// How long one key-set fetch may take. idtoken bounds it by the request's context; a hyper
/// handler future is not reliably dropped when its client goes away, so the fetch carries its own
/// deadline.
const CERT_FETCH_TIMEOUT: Duration = Duration::from_secs(10);

/// Go `base64.RawURLEncoding` for decoding: URL alphabet, no padding, non-zero trailing bits
/// accepted.
const RAW_URL_ENCODING: GeneralPurpose = GeneralPurpose::new(
    &alphabet::URL_SAFE,
    GeneralPurposeConfig::new()
        .with_encode_padding(false)
        .with_decode_padding_mode(DecodePaddingMode::RequireNone)
        .with_decode_allow_trailing_bits(true),
);

/// Port of idtoken `Payload`: a token's standard claims, plus every claim in [`Payload::claims`].
#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct Payload {
    /// `iss`.
    pub(crate) issuer: String,
    /// `aud`.
    pub(crate) audience: String,
    /// `exp` (Unix seconds).
    pub(crate) expires: i64,
    /// `iat` (Unix seconds).
    pub(crate) issued_at: i64,
    /// `sub`.
    pub(crate) subject: String,
    /// The whole claims object.
    pub(crate) claims: Map<String, Value>,
}

/// The typed half of idtoken's payload decode (Go `json.Unmarshal` into `Payload`; an absent or
/// `null` field keeps its zero value).
#[derive(Deserialize)]
struct StandardClaims {
    #[serde(default)]
    iss: Option<String>,
    #[serde(default)]
    aud: Option<String>,
    #[serde(default)]
    exp: Option<i64>,
    #[serde(default)]
    iat: Option<i64>,
    #[serde(default)]
    sub: Option<String>,
}

/// idtoken `jwtHeader`.
#[derive(Deserialize)]
struct JwtHeader {
    #[serde(default)]
    alg: Option<String>,
    #[serde(default)]
    kid: Option<String>,
}

/// idtoken `certResponse`.
#[derive(Debug, Default, Deserialize)]
struct CertResponse {
    #[serde(default)]
    keys: Option<Vec<Jwk>>,
}

/// idtoken `jwk` (the fields it reads).
#[derive(Debug, Clone, Default, Deserialize)]
struct Jwk {
    #[serde(default)]
    kid: Option<String>,
    #[serde(default)]
    e: Option<String>,
    #[serde(default)]
    n: Option<String>,
    #[serde(default)]
    x: Option<String>,
    #[serde(default)]
    y: Option<String>,
}

/// Port of idtoken `Validator`: verifies Google ID tokens with cached key sets.
pub(crate) struct Validator {
    client: CachingClient,
    sa_certs_url: String,
    iap_certs_url: String,
}

/// idtoken `defaultValidator`: the process-wide validator (and key-set cache) every
/// [`google_oidc`](super::google_oidc) authenticator shares.
static DEFAULT_VALIDATOR: LazyLock<Validator> = LazyLock::new(|| {
    Validator::with_cert_urls(
        reqwest::Client::new(),
        GOOGLE_SA_CERTS_URL,
        GOOGLE_IAP_CERTS_URL,
    )
});

/// Port of idtoken `Validate`: [`Validator::validate`] on the process-wide validator.
pub(crate) async fn validate(id_token: &str, audience: &str) -> Result<Payload, String> {
    DEFAULT_VALIDATOR.validate(id_token, audience).await
}

impl Validator {
    /// A validator that fetches the RS256 key set from `sa_certs_url` and the ES256 key set from
    /// `iap_certs_url` (tests point both at a local server).
    pub(crate) fn with_cert_urls(
        client: reqwest::Client,
        sa_certs_url: &str,
        iap_certs_url: &str,
    ) -> Self {
        Self {
            client: CachingClient::new(client),
            sa_certs_url: sa_certs_url.to_owned(),
            iap_certs_url: iap_certs_url.to_owned(),
        }
    }

    /// This validator with its key-set fetches bounded by `timeout` instead of
    /// [`CERT_FETCH_TIMEOUT`].
    #[cfg(test)]
    fn with_fetch_timeout(mut self, timeout: Duration) -> Self {
        self.client.fetch_timeout = timeout;
        self
    }

    /// Port of idtoken `Validator.validate`: decodes the token, checks the audience (skipped when
    /// `audience` is empty) and expiry, then the signature (RS256 or ES256 only). The issuer is
    /// left to the caller.
    pub(crate) async fn validate(&self, id_token: &str, audience: &str) -> Result<Payload, String> {
        let jwt = parse_jwt(id_token)?;
        let header = jwt.parsed_header()?;
        let payload = jwt.parsed_payload()?;
        let sig = jwt.decoded_signature()?;

        if !audience.is_empty() && payload.audience != audience {
            return Err("idtoken: audience provided does not match aud claim in the JWT".into());
        }

        let now = unix_now();
        if now > payload.expires {
            return Err(format!(
                "idtoken: token expired: now={now}, expires={}",
                payload.expires
            ));
        }

        let kid = header.kid.unwrap_or_default();
        let alg = header.alg.unwrap_or_default();
        match alg.as_str() {
            "RS256" => {
                self.validate_rs256(&kid, &jwt.signed_content(), &sig)
                    .await?
            }
            "ES256" => {
                self.validate_es256(&kid, &jwt.signed_content(), &sig)
                    .await?
            }
            _ => {
                return Err(format!(
                    "idtoken: expected JWT signed with RS256 or ES256 but found {}",
                    go_quote(&alg)
                ));
            }
        }
        Ok(payload)
    }

    /// idtoken `validateRS256`: PKCS #1 v1.5 with SHA-256 against the key set's `n`/`e`.
    async fn validate_rs256(&self, key_id: &str, content: &[u8], sig: &[u8]) -> Result<(), String> {
        let certs = self.client.get_cert(&self.sa_certs_url).await?;
        let j = find_matching_key(&certs, key_id)?;
        let n = decode(j.n.as_deref().unwrap_or_default())?;
        let e = decode(j.e.as_deref().unwrap_or_default())?;
        let key = DecodingKey::from_rsa_raw_components(&n, &e);
        if verify(sig, content, &key, Algorithm::RS256) {
            Ok(())
        } else {
            Err("crypto/rsa: verification error".into())
        }
    }

    /// idtoken `validateES256`: ECDSA P-256 with SHA-256 against the key set's `x`/`y`, the
    /// signature being `r || s`. (idtoken panics on a signature shorter than 32 bytes; this
    /// reports it as not valid.)
    async fn validate_es256(&self, key_id: &str, content: &[u8], sig: &[u8]) -> Result<(), String> {
        let certs = self.client.get_cert(&self.iap_certs_url).await?;
        let j = find_matching_key(&certs, key_id)?;
        let x = decode(j.x.as_deref().unwrap_or_default())?;
        let y = decode(j.y.as_deref().unwrap_or_default())?;
        let key = DecodingKey::from_ec_components(
            &RAW_URL_ENCODING.encode(x),
            &RAW_URL_ENCODING.encode(y),
        )
        .map_err(|e| e.to_string())?;
        if verify(sig, content, &key, Algorithm::ES256) {
            Ok(())
        } else {
            Err("idtoken: ES256 signature not valid".into())
        }
    }
}

/// Whether `sig` is a valid `alg` signature of `content` under `key`.
fn verify(sig: &[u8], content: &[u8], key: &DecodingKey, alg: Algorithm) -> bool {
    jsonwebtoken::crypto::verify(&RAW_URL_ENCODING.encode(sig), content, key, alg).unwrap_or(false)
}

/// idtoken `findMatchingKey`.
fn find_matching_key(response: &CertResponse, key_id: &str) -> Result<Jwk, String> {
    response
        .keys
        .iter()
        .flatten()
        .find(|k| k.kid.as_deref().unwrap_or_default() == key_id)
        .cloned()
        .ok_or_else(|| "idtoken: could not find matching cert keyId for the token provided".into())
}

/// idtoken `jwt`: the three dot-separated segments of a compact JWS.
struct Jwt<'a> {
    header: &'a str,
    payload: &'a str,
    signature: &'a str,
}

/// idtoken `parseJWT`.
fn parse_jwt(id_token: &str) -> Result<Jwt<'_>, String> {
    let segments: Vec<&str> = id_token.split('.').collect();
    match segments.as_slice() {
        [header, payload, signature] => Ok(Jwt {
            header,
            payload,
            signature,
        }),
        _ => Err(format!(
            "idtoken: invalid token, token must have three segments; found {}",
            segments.len()
        )),
    }
}

impl Jwt<'_> {
    /// idtoken `parsedHeader`.
    fn parsed_header(&self) -> Result<JwtHeader, String> {
        let dh = decode(self.header)
            .map_err(|e| format!("idtoken: unable to decode JWT header: {e}"))?;
        serde_json::from_slice(&dh)
            .map_err(|e| format!("idtoken: unable to unmarshal JWT header: {e}"))
    }

    /// idtoken `parsedPayload`.
    fn parsed_payload(&self) -> Result<Payload, String> {
        let dp = decode(self.payload)
            .map_err(|e| format!("idtoken: unable to decode JWT payload: {e}"))?;
        let std: StandardClaims = serde_json::from_slice(&dp)
            .map_err(|e| format!("idtoken: unable to unmarshal JWT payload: {e}"))?;
        let claims: Map<String, Value> = serde_json::from_slice(&dp)
            .map_err(|e| format!("idtoken: unable to unmarshal JWT payload claims: {e}"))?;
        Ok(Payload {
            issuer: std.iss.unwrap_or_default(),
            audience: std.aud.unwrap_or_default(),
            expires: std.exp.unwrap_or_default(),
            issued_at: std.iat.unwrap_or_default(),
            subject: std.sub.unwrap_or_default(),
            claims,
        })
    }

    /// idtoken `decodedSignature`.
    fn decoded_signature(&self) -> Result<Vec<u8>, String> {
        decode(self.signature).map_err(|e| format!("idtoken: unable to decode JWT signature: {e}"))
    }

    /// The signed bytes, `header "." payload` (idtoken hashes these in `hashedContent`).
    fn signed_content(&self) -> Vec<u8> {
        format!("{}.{}", self.header, self.payload).into_bytes()
    }
}

/// idtoken `decode`: `base64.RawURLEncoding`.
fn decode(s: &str) -> Result<Vec<u8>, String> {
    RAW_URL_ENCODING.decode(s).map_err(|e| e.to_string())
}

/// Go `time.Now().Unix()`.
fn unix_now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX))
        .unwrap_or_default()
}

/// A key-set fetch in progress, shared by every caller that misses the cache meanwhile.
type InFlight = Shared<BoxFuture<'static, Result<Arc<CertResponse>, String>>>;

/// Port of idtoken `cachingClient`: key sets cached per URL until their expiry. Concurrent misses
/// on one URL share a single fetch (idtoken fetches once per caller; tokens are checked for
/// audience and expiry before the fetch, so unauthenticated callers can reach it, and the shared
/// fetch keeps them from piling up requests to Google).
struct CachingClient {
    client: reqwest::Client,
    certs: Arc<Mutex<HashMap<String, CachedResponse>>>,
    in_flight: Mutex<HashMap<String, InFlight>>,
    fetch_timeout: Duration,
}

/// idtoken `cachedResponse`.
struct CachedResponse {
    resp: Arc<CertResponse>,
    exp: Instant,
}

impl CachingClient {
    fn new(client: reqwest::Client) -> Self {
        Self {
            client,
            certs: Arc::new(Mutex::new(HashMap::with_capacity(2))),
            in_flight: Mutex::new(HashMap::with_capacity(2)),
            fetch_timeout: CERT_FETCH_TIMEOUT,
        }
    }

    /// idtoken `getCert`: the cached key set of `url`, else a fresh fetch, joining one already in
    /// progress for `url`.
    async fn get_cert(&self, url: &str) -> Result<Arc<CertResponse>, String> {
        if let Some(resp) = get(&self.certs, url) {
            return Ok(resp);
        }
        let fetch = {
            let mut in_flight = self.in_flight.lock();
            match in_flight.get(url) {
                // A finished entry is left over from callers that went away before it completed.
                Some(f) if f.peek().is_none() => f.clone(),
                _ => {
                    // Spawned, so the fetch completes (within its timeout) and is cached even if
                    // every waiting caller goes away.
                    let task = tokio::spawn(fetch_cert(
                        self.client.clone(),
                        self.certs.clone(),
                        url.to_owned(),
                        self.fetch_timeout,
                    ));
                    let f = async move { task.await.unwrap_or_else(|e| Err(e.to_string())) }
                        .boxed()
                        .shared();
                    in_flight.insert(url.to_owned(), f.clone());
                    f
                }
            }
        };
        let result = fetch.clone().await;
        let mut in_flight = self.in_flight.lock();
        if in_flight.get(url).is_some_and(|f| f.ptr_eq(&fetch)) {
            in_flight.remove(url);
        }
        result
    }
}

/// The GET half of idtoken `getCert`: only a 200 is accepted, the body's first JSON value is
/// decoded and cached per the response headers. The whole fetch is bounded by `timeout`.
async fn fetch_cert(
    client: reqwest::Client,
    certs: Arc<Mutex<HashMap<String, CachedResponse>>>,
    url: String,
    timeout: Duration,
) -> Result<Arc<CertResponse>, String> {
    let resp = client
        .get(&url)
        .timeout(timeout)
        .send()
        .await
        .map_err(|e| e.to_string())?;
    if resp.status() != reqwest::StatusCode::OK {
        return Err(format!(
            "idtoken: unable to retrieve cert, got status code {}",
            resp.status().as_u16()
        ));
    }
    let headers = resp.headers().clone();
    let body = resp.bytes().await.map_err(|e| e.to_string())?;
    // Go `json.NewDecoder(body).Decode`: the first value; trailing bytes are not read.
    let cert_resp = match serde_json::Deserializer::from_slice(&body)
        .into_iter::<CertResponse>()
        .next()
    {
        Some(Ok(c)) => Arc::new(c),
        Some(Err(e)) => return Err(e.to_string()),
        None => return Err("EOF".into()),
    };
    set(&certs, &url, cert_resp.clone(), &headers);
    Ok(cert_resp)
}

/// idtoken `cachingClient.get`: a hit until the current time is after the entry's expiry.
fn get(certs: &Mutex<HashMap<String, CachedResponse>>, url: &str) -> Option<Arc<CertResponse>> {
    let certs = certs.lock();
    let cached = certs.get(url)?;
    if Instant::now() > cached.exp {
        return None;
    }
    Some(cached.resp.clone())
}

/// idtoken `cachingClient.set`.
fn set(
    certs: &Mutex<HashMap<String, CachedResponse>>,
    url: &str,
    resp: Arc<CertResponse>,
    headers: &http::HeaderMap,
) {
    let exp = calculate_expire_time(Instant::now(), headers);
    certs
        .lock()
        .insert(url.to_owned(), CachedResponse { resp, exp });
}

/// Port of idtoken `calculateExpireTime`: `now` plus the last `max-age` directive of the first
/// `Cache-Control` header, less `Age`. A directive or `Age` that does not parse expires the entry
/// at once (`now`); no `max-age` is zero.
fn calculate_expire_time(now: Instant, headers: &http::HeaderMap) -> Instant {
    let header = |name: &str| {
        headers
            .get(name)
            .and_then(|v| v.to_str().ok())
            .unwrap_or_default()
            .to_owned()
    };
    let mut max_age: i64 = 0;
    for v in header("cache-control").split(',') {
        if v.contains("max-age") {
            let ss: Vec<&str> = v.split('=').collect();
            let Some(value) = ss.get(1) else {
                return now;
            };
            let Ok(ma) = value.parse::<i64>() else {
                return now;
            };
            max_age = ma;
        }
    }
    let a = header("age");
    if a.is_empty() {
        return add_secs(now, max_age);
    }
    let Ok(age) = a.parse::<i64>() else {
        return now;
    };
    add_secs(now, max_age.saturating_sub(age))
}

/// `now` moved by `secs` seconds (negative moves it back), clamped to what `Instant` can hold.
fn add_secs(now: Instant, secs: i64) -> Instant {
    let d = Duration::from_secs(secs.unsigned_abs());
    if secs >= 0 {
        now.checked_add(d).unwrap_or(now)
    } else {
        now.checked_sub(d).unwrap_or(now)
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    //! The verifier against a 127.0.0.1 key-set server: parsing, caching, RS256 and ES256, and
    //! every idtoken rejection. Keys are the PEM files under `testdata/oidc/`.

    use std::sync::atomic::{AtomicUsize, Ordering};

    use axum::Router;
    use axum::response::IntoResponse;
    use axum::routing::get;
    use http::{HeaderMap, HeaderValue};
    use jsonwebtoken::jwk::{AlgorithmParameters, Jwk as JwtJwk};
    use jsonwebtoken::{EncodingKey, Header};
    use pretty_assertions::assert_eq;
    use serde_json::json;

    use super::*;

    const RSA_PEM: &[u8] = include_bytes!("../../testdata/oidc/rsa_private.pem");
    const EC_PEM: &[u8] = include_bytes!("../../testdata/oidc/ec_private.pem");
    const AUD: &str = "https://example-agent.example.com";

    fn rsa_key() -> EncodingKey {
        EncodingKey::from_rsa_pem(RSA_PEM).unwrap()
    }

    fn ec_key() -> EncodingKey {
        EncodingKey::from_ec_pem(EC_PEM).unwrap()
    }

    /// The public JWK of `key` under `kid`, in Google's key-set shape.
    fn public_jwk(key: &EncodingKey, alg: Algorithm, kid: &str) -> Value {
        let jwk = JwtJwk::from_encoding_key(key, alg).unwrap();
        match jwk.algorithm {
            AlgorithmParameters::RSA(p) => {
                json!({"alg": "RS256", "kty": "RSA", "use": "sig", "kid": kid, "n": p.n, "e": p.e})
            }
            AlgorithmParameters::EllipticCurve(p) => {
                json!({"alg": "ES256", "kty": "EC", "crv": "P-256", "kid": kid, "x": p.x, "y": p.y})
            }
            other => panic!("unexpected key parameters {other:?}"),
        }
    }

    /// A 127.0.0.1 server answering `/sa` and `/iap` with fixed bodies and headers, counting hits.
    struct KeyServer {
        base: String,
        sa_hits: Arc<AtomicUsize>,
        iap_hits: Arc<AtomicUsize>,
    }

    #[derive(Clone)]
    struct Reply {
        status: http::StatusCode,
        body: String,
        cache_control: Option<&'static str>,
    }

    impl Reply {
        fn ok(body: Value, cache_control: Option<&'static str>) -> Self {
            Self {
                status: http::StatusCode::OK,
                body: body.to_string(),
                cache_control,
            }
        }
    }

    async fn key_server(sa: Reply, iap: Reply) -> KeyServer {
        let sa_hits = Arc::new(AtomicUsize::new(0));
        let iap_hits = Arc::new(AtomicUsize::new(0));
        let route = |reply: Reply, hits: Arc<AtomicUsize>| {
            get(move || {
                let reply = reply.clone();
                let hits = hits.clone();
                async move {
                    hits.fetch_add(1, Ordering::SeqCst);
                    let mut headers = HeaderMap::new();
                    if let Some(cc) = reply.cache_control {
                        headers.insert("cache-control", HeaderValue::from_static(cc));
                    }
                    (reply.status, headers, reply.body).into_response()
                }
            })
        };
        let app = Router::new()
            .route("/sa", route(sa, sa_hits.clone()))
            .route("/iap", route(iap, iap_hits.clone()));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await });
        KeyServer {
            base: format!("http://{addr}"),
            sa_hits,
            iap_hits,
        }
    }

    impl KeyServer {
        fn validator(&self) -> Validator {
            Validator::with_cert_urls(
                reqwest::Client::new(),
                &format!("{}/sa", self.base),
                &format!("{}/iap", self.base),
            )
        }
    }

    fn claims(aud: &str, exp: i64) -> Value {
        json!({
            "iss": "https://accounts.google.com",
            "aud": aud,
            "sub": "108100000000000000000",
            "iat": unix_now() - 10,
            "exp": exp,
            "email": "pubsub-push@example-project.iam.gserviceaccount.com",
            "email_verified": true,
        })
    }

    fn sign(alg: Algorithm, key: &EncodingKey, kid: &str, claims: &Value) -> String {
        let mut header = Header::new(alg);
        header.kid = Some(kid.to_owned());
        jsonwebtoken::encode(&header, claims, key).unwrap()
    }

    fn rsa_set() -> Value {
        json!({"keys": [public_jwk(&rsa_key(), Algorithm::RS256, "rsa-1")]})
    }

    fn ec_set() -> Value {
        json!({"keys": [public_jwk(&ec_key(), Algorithm::ES256, "ec-1")]})
    }

    #[tokio::test]
    async fn rs256_token_verifies_and_the_key_set_is_cached() {
        let srv = key_server(
            Reply::ok(rsa_set(), Some("public, max-age=3600, must-revalidate")),
            Reply::ok(ec_set(), None),
        )
        .await;
        let v = srv.validator();
        let want = claims(AUD, unix_now() + 3600);
        let token = sign(Algorithm::RS256, &rsa_key(), "rsa-1", &want);

        let p = v.validate(&token, AUD).await.unwrap();
        assert_eq!(p.issuer, "https://accounts.google.com");
        assert_eq!(p.audience, AUD);
        assert_eq!(p.subject, "108100000000000000000");
        assert_eq!(p.expires, want["exp"].as_i64().unwrap());
        assert_eq!(p.issued_at, want["iat"].as_i64().unwrap());
        assert_eq!(Value::Object(p.claims), want);

        v.validate(&token, AUD).await.unwrap();
        assert_eq!(
            srv.sa_hits.load(Ordering::SeqCst),
            1,
            "a cached key set is not refetched"
        );
        assert_eq!(srv.iap_hits.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn es256_token_verifies_against_the_iap_key_set() {
        let srv = key_server(
            Reply::ok(rsa_set(), None),
            Reply::ok(ec_set(), Some("max-age=60")),
        )
        .await;
        let v = srv.validator();
        let token = sign(
            Algorithm::ES256,
            &ec_key(),
            "ec-1",
            &claims(AUD, unix_now() + 60),
        );

        let p = v.validate(&token, AUD).await.unwrap();
        assert_eq!(p.subject, "108100000000000000000");
        assert_eq!(srv.iap_hits.load(Ordering::SeqCst), 1);
        assert_eq!(srv.sa_hits.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn an_expired_cache_entry_is_refetched() {
        // max-age=0: the entry expires as it is stored.
        let srv = key_server(
            Reply::ok(rsa_set(), Some("max-age=0")),
            Reply::ok(ec_set(), None),
        )
        .await;
        let v = srv.validator();
        let token = sign(
            Algorithm::RS256,
            &rsa_key(),
            "rsa-1",
            &claims(AUD, unix_now() + 60),
        );
        v.validate(&token, AUD).await.unwrap();
        tokio::time::sleep(Duration::from_millis(5)).await;
        v.validate(&token, AUD).await.unwrap();
        assert_eq!(srv.sa_hits.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn rejections_carry_idtoken_text() {
        let srv = key_server(Reply::ok(rsa_set(), None), Reply::ok(ec_set(), None)).await;
        let v = srv.validator();
        let live = unix_now() + 60;
        let rsa = |kid: &str, c: &Value| sign(Algorithm::RS256, &rsa_key(), kid, c);

        let err = |r: Result<Payload, String>| r.unwrap_err();
        assert_eq!(
            err(v.validate("a.b", AUD).await),
            "idtoken: invalid token, token must have three segments; found 2"
        );
        assert!(
            err(v.validate("!.e30.AA", AUD).await)
                .starts_with("idtoken: unable to decode JWT header: ")
        );
        assert!(
            err(v.validate("e30.!.AA", AUD).await)
                .starts_with("idtoken: unable to decode JWT payload: ")
        );
        // `aud` must be a string (Go's `Payload.Audience`).
        let arr = format!("e30.{}.AA", RAW_URL_ENCODING.encode(br#"{"aud":["x"]}"#));
        assert!(
            err(v.validate(&arr, AUD).await)
                .starts_with("idtoken: unable to unmarshal JWT payload: ")
        );
        assert!(
            err(v.validate("e30.e30.!", AUD).await)
                .starts_with("idtoken: unable to decode JWT signature: ")
        );
        assert_eq!(
            err(v
                .validate(
                    &rsa("rsa-1", &claims("https://other.example.com", live)),
                    AUD
                )
                .await),
            "idtoken: audience provided does not match aud claim in the JWT"
        );
        // An empty audience skips the audience check.
        v.validate(
            &rsa("rsa-1", &claims("https://other.example.com", live)),
            "",
        )
        .await
        .unwrap();
        assert!(
            err(v
                .validate(&rsa("rsa-1", &claims(AUD, unix_now() - 5)), AUD)
                .await)
            .starts_with("idtoken: token expired: now=")
        );
        let hs = jsonwebtoken::encode(
            &Header::new(Algorithm::HS256),
            &claims(AUD, live),
            &EncodingKey::from_secret(b"secret"),
        )
        .unwrap();
        assert_eq!(
            err(v.validate(&hs, AUD).await),
            r#"idtoken: expected JWT signed with RS256 or ES256 but found "HS256""#
        );
        assert_eq!(
            err(v.validate(&rsa("unknown", &claims(AUD, live)), AUD).await),
            "idtoken: could not find matching cert keyId for the token provided"
        );
        // A signature that does not match the content.
        let good = rsa("rsa-1", &claims(AUD, live));
        let (signed, _) = good.rsplit_once('.').unwrap();
        let other = rsa("rsa-1", &claims(AUD, live + 1));
        let (_, other_sig) = other.rsplit_once('.').unwrap();
        assert_eq!(
            err(v.validate(&format!("{signed}.{other_sig}"), AUD).await),
            "crypto/rsa: verification error"
        );
        let es = sign(Algorithm::ES256, &ec_key(), "ec-1", &claims(AUD, live));
        let (es_signed, _) = es.rsplit_once('.').unwrap();
        assert_eq!(
            err(v.validate(&format!("{es_signed}.AAAA"), AUD).await),
            "idtoken: ES256 signature not valid"
        );
    }

    #[tokio::test]
    async fn a_failed_certificate_fetch_is_an_error() {
        let srv = key_server(
            Reply {
                status: http::StatusCode::INTERNAL_SERVER_ERROR,
                body: String::new(),
                cache_control: None,
            },
            Reply::ok(json!({"keys": "not-a-list"}), None),
        )
        .await;
        let v = srv.validator();
        let live = unix_now() + 60;
        let token = sign(Algorithm::RS256, &rsa_key(), "rsa-1", &claims(AUD, live));
        assert_eq!(
            v.validate(&token, AUD).await.unwrap_err(),
            "idtoken: unable to retrieve cert, got status code 500"
        );
        let es = sign(Algorithm::ES256, &ec_key(), "ec-1", &claims(AUD, live));
        assert!(
            v.validate(&es, AUD)
                .await
                .unwrap_err()
                .contains("invalid type")
        );
    }

    /// [`GoogleOidc`](super::super::google_oidc::GoogleOidc) with this validator behind
    /// [`middleware`](super::super::middleware): the answer's status and the caller's user ID.
    async fn through_middleware(
        v: Arc<Validator>,
        token: &str,
    ) -> (http::StatusCode, Option<String>) {
        use tower::{Layer as _, ServiceExt as _};

        let validate: super::super::google_oidc::TokenValidator =
            Arc::new(move |t: String, a: String| {
                let v = v.clone();
                Box::pin(async move { v.validate(&t, &a).await.map(Some) })
            });
        let g = super::super::google_oidc::GoogleOidc {
            audience: AUD.into(),
            allowed_service_accounts: vec![
                "pubsub-push@example-project.iam.gserviceaccount.com".into(),
            ],
            validate: Some(validate),
            log: None,
        };
        let next = tower::service_fn(|req: http::Request<axum::body::Body>| async move {
            let user = req
                .extensions()
                .get::<super::super::Caller>()
                .map(|c| c.user_id.clone())
                .unwrap_or_default();
            Ok::<_, std::convert::Infallible>(axum::response::Response::new(
                axum::body::Body::from(user),
            ))
        });
        let req = http::Request::post("/apps/a/trigger/pubsub")
            .header(http::header::AUTHORIZATION, format!("Bearer {token}"))
            .body(axum::body::Body::empty())
            .unwrap();
        let resp = super::super::middleware(Arc::new(g))
            .layer(next)
            .oneshot(req)
            .await
            .unwrap();
        let status = resp.status();
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let body = String::from_utf8(body.to_vec()).unwrap();
        (status, (status == http::StatusCode::OK).then_some(body))
    }

    #[tokio::test]
    async fn google_oidc_over_a_local_key_set() {
        let srv = key_server(
            Reply::ok(rsa_set(), Some("max-age=60")),
            Reply::ok(ec_set(), None),
        )
        .await;
        let v = Arc::new(srv.validator());
        let token = sign(
            Algorithm::RS256,
            &rsa_key(),
            "rsa-1",
            &claims(AUD, unix_now() + 60),
        );
        assert_eq!(
            through_middleware(v.clone(), &token).await,
            (
                http::StatusCode::OK,
                Some("108100000000000000000".to_owned())
            )
        );
        let es = sign(
            Algorithm::ES256,
            &ec_key(),
            "ec-1",
            &claims(AUD, unix_now() + 60),
        );
        assert_eq!(
            through_middleware(v.clone(), &es).await,
            (
                http::StatusCode::OK,
                Some("108100000000000000000".to_owned())
            )
        );
        let foreign = sign(
            Algorithm::RS256,
            &rsa_key(),
            "rsa-1",
            &claims("https://other.example.com", unix_now() + 60),
        );
        assert_eq!(
            through_middleware(v, &foreign).await,
            (http::StatusCode::UNAUTHORIZED, None)
        );
    }

    /// A key-set endpoint that accepts connections and never answers: the fetch gives up after
    /// the client's timeout instead of holding the request open.
    #[tokio::test]
    async fn a_hanging_certificate_endpoint_times_out() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let mut held = Vec::new();
            loop {
                let (conn, _) = listener.accept().await.unwrap();
                held.push(conn);
            }
        });
        let v = Validator::with_cert_urls(
            reqwest::Client::new(),
            &format!("http://{addr}/sa"),
            &format!("http://{addr}/iap"),
        )
        .with_fetch_timeout(Duration::from_millis(200));
        let token = sign(
            Algorithm::RS256,
            &rsa_key(),
            "rsa-1",
            &claims(AUD, unix_now() + 60),
        );
        let r = tokio::time::timeout(Duration::from_secs(5), v.validate(&token, AUD))
            .await
            .expect("the certificate fetch must time out on its own");
        assert!(r.is_err());
    }

    /// Concurrent cache misses on one URL collapse into a single fetch whose answer they share
    /// (max-age=0, so a waiter could not just find the entry in the cache afterwards).
    #[tokio::test]
    async fn concurrent_cache_misses_share_one_fetch() {
        let hits = Arc::new(AtomicUsize::new(0));
        let body = rsa_set().to_string();
        let counted = hits.clone();
        let app = Router::new().route(
            "/sa",
            get(move || {
                let hits = counted.clone();
                let body = body.clone();
                async move {
                    hits.fetch_add(1, Ordering::SeqCst);
                    tokio::time::sleep(Duration::from_millis(200)).await;
                    ([("cache-control", "max-age=0")], body).into_response()
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await });
        let v = Arc::new(Validator::with_cert_urls(
            reqwest::Client::new(),
            &format!("http://{addr}/sa"),
            &format!("http://{addr}/iap"),
        ));
        let token = sign(
            Algorithm::RS256,
            &rsa_key(),
            "rsa-1",
            &claims(AUD, unix_now() + 60),
        );
        let calls = (0..8).map(|_| {
            let v = v.clone();
            let token = token.clone();
            tokio::spawn(async move { v.validate(&token, AUD).await })
        });
        for r in futures::future::join_all(calls).await {
            r.unwrap().unwrap();
        }
        assert_eq!(hits.load(Ordering::SeqCst), 1);
    }

    /// Go answers a signing-certificate fetch failure 401, not 500 (googleoidc.go `deny`).
    #[tokio::test]
    async fn a_certificate_fetch_failure_is_401() {
        let srv = key_server(
            Reply {
                status: http::StatusCode::SERVICE_UNAVAILABLE,
                body: String::new(),
                cache_control: None,
            },
            Reply::ok(ec_set(), None),
        )
        .await;
        let token = sign(
            Algorithm::RS256,
            &rsa_key(),
            "rsa-1",
            &claims(AUD, unix_now() + 60),
        );
        assert_eq!(
            through_middleware(Arc::new(srv.validator()), &token).await,
            (http::StatusCode::UNAUTHORIZED, None)
        );
        // Nothing listens on the URL at all.
        let v = Validator::with_cert_urls(
            reqwest::Client::new(),
            "http://127.0.0.1:1/sa",
            "http://127.0.0.1:1/iap",
        );
        assert_eq!(
            through_middleware(Arc::new(v), &token).await,
            (http::StatusCode::UNAUTHORIZED, None)
        );
    }

    #[test]
    fn expire_time_follows_cache_control_and_age() {
        let now = Instant::now();
        let at = |pairs: &[(&'static str, &'static str)]| {
            let mut h = HeaderMap::new();
            for (k, v) in pairs {
                h.append(*k, HeaderValue::from_static(v));
            }
            calculate_expire_time(now, &h)
        };
        let secs = Duration::from_secs;
        assert_eq!(at(&[]), now);
        assert_eq!(
            at(&[("cache-control", "public, max-age=3600")]),
            now + secs(3600)
        );
        assert_eq!(
            at(&[("cache-control", "max-age=100, s-maxage=5")]),
            now + secs(100),
            "a later directive without max-age leaves it"
        );
        assert_eq!(
            at(&[("cache-control", "max-age=100"), ("age", "40")]),
            now + secs(60)
        );
        assert_eq!(at(&[("cache-control", "max-age"), ("age", "40")]), now);
        assert_eq!(at(&[("cache-control", "max-age=soon")]), now);
        assert_eq!(
            at(&[("cache-control", "max-age=100 ")]),
            now,
            "Go Atoi rejects the space"
        );
        assert_eq!(at(&[("cache-control", "max-age=100"), ("age", "x")]), now);
        assert_eq!(
            at(&[("cache-control", "max-age=10"), ("age", "40")]),
            now - secs(30)
        );
        assert_eq!(
            at(&[
                ("cache-control", "no-cache"),
                ("cache-control", "max-age=100")
            ]),
            now,
            "only the first Cache-Control header is read"
        );
    }
}
