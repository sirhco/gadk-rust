// Copyright 2026 Chris Olson
// Portions derived from ADK-Go (https://github.com/google/adk-go), Copyright 2026 Google LLC.
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

//! The credential-service client: construction, the retrieval loop, credential mapping and the JSON
//! POST every call goes through. The two services' request and response shapes live in
//! `agent_identity.rs` and `connector.rs`.

use std::collections::BTreeMap;
use std::fmt;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, LazyLock};
use std::time::Duration;

use chrono::{DateTime, Utc};
use gadk_agent::go_quote;
use gadk_platform::go_parse_rfc3339;
use sha2::{Digest, Sha256};
use tokio::time::Instant;

use super::provider::ProviderScheme;
use super::redact::{go_to_lower, redacted_for_error};
use super::{ApiError, Error, ErrorKind};
use crate::credential::{ApiKey, Bearer, Credential, with_headers};
use crate::error::ConsentRequiredError;
use crate::go_fmt::hex_encode;
use crate::provider::AuthContext;
use crate::providers::{AdcSource, adc_token_source, token_source};

/// The [`Client`] configuration. Empty strings and a zero `poll_timeout` mean the default.
#[derive(Clone, Default)]
pub struct Config {
    /// The HTTP client: used verbatim (no ADC, no redirect policy added) and must carry its own
    /// credentials, e.g. `auth::client(...)`; `None` builds the ADC client (cloud-platform,
    /// `redirect::Policy::none()`). A plain `reqwest::Client` converts with `.into()`.
    ///
    /// It must authenticate as one identity for the client's whole life. A [`Client`] is a
    /// credential-cache dimension because it is assumed to fix who the service mints a
    /// credential for, so build one `Client` per identity and share it.
    pub http_client: Option<reqwest_middleware::ClientWithMiddleware>,
    /// The Agent Identity endpoint (trailing '/' trimmed). Used as given, not parsed: an
    /// `http://` value sends the ADC token in the clear, so keep it https outside tests.
    pub agent_identity_endpoint: String,
    /// The IAM Connector endpoint (trailing '/' trimmed), with the same caveat.
    pub connector_endpoint: String,
    /// Bounds the time spent retrying a pending retrieval. It caps the retry loop, not one request;
    /// bound a single request with the HTTP client's own timeout.
    pub poll_timeout: Duration,
}

impl fmt::Debug for Config {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Config")
            .field(
                "http_client",
                &self.http_client.as_ref().map(|_| "ClientWithMiddleware"),
            )
            .field("agent_identity_endpoint", &self.agent_identity_endpoint)
            .field("connector_endpoint", &self.connector_endpoint)
            .field("poll_timeout", &self.poll_timeout)
            .finish()
    }
}

/// Retrieves end-user credentials from the Agent Identity and IAM Connector credential services and
/// maps them to [`crate::Credential`]. Only [`Client::new`] (and the crate's `with_adc`) build one.
pub struct Client {
    pub(crate) http: reqwest_middleware::ClientWithMiddleware,
    pub(crate) agent_identity_url: String,
    pub(crate) connector_url: String,
    pub(crate) poll_timeout: Duration,
    pub(crate) initial_backoff: Duration,
    /// Names this client for the credential cache. What the services return depends on the endpoint
    /// asked and on the identity the ask authenticates as, and this package can observe neither, so
    /// every client is its own slot. It is unique per process too
    /// (`join_fields([CLIENT_NONCE, seq])`), so a store that outlives the process misses rather
    /// than serving an entry no live client wrote.
    pub(crate) cache_slot: String,
}

impl fmt::Debug for Client {
    // Endpoints and timeouts only.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Client")
            .field("agent_identity_url", &self.agent_identity_url)
            .field("connector_url", &self.connector_url)
            .field("poll_timeout", &self.poll_timeout)
            .field("initial_backoff", &self.initial_backoff)
            .finish_non_exhaustive()
    }
}

impl Client {
    /// Builds a client (with `AdcSource::Discover`). Unless `cfg.http_client` is
    /// set, it discovers Application Default Credentials (cloud-platform scope) to authenticate
    /// calls to the services, on an HTTP client that refuses redirects. ADC failure:
    /// `"gcp: find default credentials: <builder error>"` (source kept).
    pub fn new(cfg: Config) -> Result<Client, Error> {
        Self::with_adc(cfg, &AdcSource::Discover)
    }

    /// The ADC branch of [`Client::new`] with an injected source (tests pass `AdcSource::Json`).
    /// Builds the HTTP client with `crate::client`, over a no-redirect `reqwest::Client`, with
    /// `crate::token_source(ts)` and `AuthContext::anonymous()`, where `ts` comes from
    /// `crate::providers::adc_token_source(adc, &[CLOUD_PLATFORM_SCOPE.into()])`.
    /// `Client::new(cfg)` is `with_adc(cfg, &AdcSource::Discover)`.
    pub(crate) fn with_adc(cfg: Config, adc: &AdcSource) -> Result<Client, Error> {
        let endpoint = |given: &str, default: &str| {
            if given.is_empty() {
                default.to_string()
            } else {
                given.trim_end_matches('/').to_string()
            }
        };
        let agent_identity_url = endpoint(&cfg.agent_identity_endpoint, DEFAULT_AGENT_IDENTITY_URL);
        let connector_url = endpoint(&cfg.connector_endpoint, DEFAULT_CONNECTOR_URL);
        let poll_timeout = if cfg.poll_timeout.is_zero() {
            DEFAULT_POLL_TIMEOUT
        } else {
            cfg.poll_timeout
        };
        let http = match cfg.http_client {
            Some(http) => http,
            None => adc_http_client(adc)?,
        };
        // The incremented value.
        let seq = CLIENT_SEQ
            .fetch_add(1, Ordering::Relaxed)
            .wrapping_add(1)
            .to_string();
        Ok(Client {
            http,
            agent_identity_url,
            connector_url,
            poll_timeout,
            initial_backoff: DEFAULT_INITIAL_BACKOFF,
            cache_slot: join_fields(&[CLIENT_NONCE.as_str(), seq.as_str()]),
        })
    }

    /// Retrieves a credential for `req`, polling while the service reports a non-interactive
    /// pending state (up to the poll timeout). Interactive consent surfaces as a
    /// [`crate::ConsentRequiredError`] reachable through `source()` ([`crate::find_consent`]); the
    /// sentinels match with [`super::is_kind`], a non-2xx status with [`super::find_api_error`].
    /// Every error past validation names the resource.
    ///
    /// Dropping the returned future abandons the retrieval, including a pending poll's wait.
    pub async fn retrieve_credential(&self, req: Request) -> Result<Retrieval, Error> {
        if req.resource.is_empty() {
            return Err(Error::new("gcp: RetrieveCredential requires a Resource"));
        }
        if req.user_id.is_empty() {
            return Err(Error::new("gcp: RetrieveCredential requires a UserID"));
        }
        validate_resource(&req.resource).map_err(|e| e.prefixed("gcp: RetrieveCredential"))?;
        // Named once here, and only the resource: it is configuration, where every other id in
        // scope comes off the request. The service text inside an error was scrubbed where it
        // entered (do_post, map_credential, the connector operation). It is deliberately not
        // scrubbed again: a second pass finds no real occurrence and can find spurious ones inside
        // the markers the first pass wrote.
        self.poll(&req)
            .await
            .map_err(|e| e.with_resource(&req.resource))
    }

    /// The retrieval loop, minus the resource decoration.
    async fn poll(&self, req: &Request) -> Result<Retrieval, Error> {
        let connector = CONNECTOR_RESOURCE_RE.is_match(&req.resource);
        // A poll timeout too large to add to now leaves no deadline, never a panic.
        let deadline = Instant::now().checked_add(self.poll_timeout);
        let mut backoff = self.initial_backoff;
        loop {
            let outcome = if connector {
                self.retrieve_connector(req).await?
            } else {
                self.retrieve_agent_identity(req).await?
            };
            match outcome {
                Outcome::Cred {
                    header,
                    token,
                    expires_at,
                } => {
                    let secrets = [
                        req.user_id.as_str(),
                        req.continue_uri.as_str(),
                        req.prior_token.as_str(),
                    ];
                    let credential = map_credential(&header, &token, &secrets)?;
                    return Ok(Retrieval {
                        credential,
                        expires_at,
                    });
                }
                Outcome::Consent { auth_uri, nonce } => {
                    let consent = ConsentRequiredError {
                        auth_uri,
                        nonce,
                        key: String::new(),
                    };
                    return Err(Error::new(consent.to_string()).caused_by(consent));
                }
                Outcome::Rejected => return Err(Error::from_kind(ErrorKind::ConsentRejected)),
                Outcome::Pending => {
                    let remaining = match deadline {
                        Some(d) => d.saturating_duration_since(Instant::now()),
                        None => self.poll_timeout,
                    };
                    if remaining.is_zero() {
                        return Err(Error::from_kind(ErrorKind::PollTimeout));
                    }
                    // The caller giving up drops this future, and the sleep with it.
                    tokio::time::sleep(backoff.min(remaining)).await;
                    backoff = backoff.saturating_mul(2).min(MAX_BACKOFF);
                }
            }
        }
    }

    /// The store key of a credential: a `CredentialKey` with the identity's `app_name` and
    /// `user_id`, and `key` set to `cache_slot(self, scheme)`. Hand it to
    /// [`crate::CredentialStore::delete`] to invalidate a credential ahead of its expiry. Only
    /// `app_name` and `user_id` take part: a credential is shared across a user's sessions. Pass
    /// the scheme the provider was built with; it is meaningful only while this client is alive.
    pub fn cache_key(
        &self,
        scheme: &ProviderScheme,
        id: &gadk_agent::Identity,
    ) -> crate::CredentialKey {
        crate::CredentialKey {
            app_name: id.app_name.clone(),
            user_id: id.user_id.clone(),
            key: cache_slot(self, scheme),
        }
    }

    /// JSON POST, 1 MiB cap, status before size, redacted ApiError,
    /// `MalformedResponse` decode error.
    pub(crate) async fn do_post<B, T>(
        &self,
        url: &str,
        body: &B,
        secrets: &[&str],
    ) -> Result<T, Error>
    where
        B: serde::Serialize + Sync,
        T: serde::de::DeserializeOwned,
    {
        let buf = go_marshal(body)
            .map_err(|e| Error::new(format!("gcp: marshal request: {e}")).caused_by(e))?;
        let target = reqwest::Url::parse(url)
            .map_err(|e| Error::new(format!("gcp: build request: {e}")).caused_by(e))?;
        let mut resp = self
            .http
            .post(target)
            .header(http::header::CONTENT_TYPE, "application/json")
            .header(http::header::ACCEPT, "application/json")
            .body(buf)
            .send()
            .await
            .map_err(|e| Error::new(format!("gcp: call credentials service: {e}")).caused_by(e))?;
        let status = resp.status();

        // Read one byte past the cap, so an oversized body is caught explicitly rather than
        // decoded as silently truncated (and thus garbled) JSON.
        let mut data: Vec<u8> = Vec::new();
        while data.len() <= MAX_BODY {
            match resp.chunk().await {
                Ok(Some(chunk)) => {
                    let room = MAX_BODY + 1 - data.len();
                    data.extend_from_slice(&chunk[..chunk.len().min(room)]);
                }
                Ok(None) => break,
                Err(e) => {
                    return Err(Error::new(format!("gcp: read response: {e}")).caused_by(e));
                }
            }
        }
        // Contract note 6: the scrub works on &str, so a non-UTF-8 byte becomes U+FFFD here.
        let text = String::from_utf8_lossy(&data);

        // The status before the size check, so an oversized error page still reports the
        // status, the most actionable field.
        if !status.is_success() {
            let api = ApiError {
                status_code: status.as_u16(),
                body: redacted_for_error(text.trim(), secrets),
            };
            return Err(Error::new(api.to_string()).caused_by(api));
        }
        if data.len() > MAX_BODY {
            return Err(Error::new(format!(
                "gcp: credentials service response exceeded {MAX_BODY} bytes"
            )));
        }
        serde_json::from_str(&text).map_err(|e| {
            // Scrubbed and quoted like the body above, and NOT carrying the decoder's error as
            // a source: its message quotes the token it choked on, which is service text, and
            // the unscrubbed original would stay reachable through source().
            let kind = ErrorKind::MalformedResponse;
            Error::with_kind(
                kind,
                format!(
                    "{kind}: {}",
                    go_quote(&redacted_for_error(&e.to_string(), secrets))
                ),
            )
        })
    }
}

/// The client for ADC authentication. The auth transport re-signs every request it sends, below the
/// layer where a redirect would strip credentials, so a followed redirect would hand the
/// cloud-platform token to whatever host it names: the client refuses them.
fn adc_http_client(adc: &AdcSource) -> Result<reqwest_middleware::ClientWithMiddleware, Error> {
    let ts = adc_token_source(adc, &[CLOUD_PLATFORM_SCOPE.to_string()]).map_err(|e| {
        // The seam already prefixes the discovery error with "auth: find default credentials", so
        // the message uses the inner text.
        let detail = match &e {
            crate::Error::FindDefaultCredentials(inner) => inner.to_string(),
            other => other.to_string(),
        };
        Error::new(format!("gcp: find default credentials: {detail}")).caused_by(e)
    })?;
    let base = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|e| Error::new(format!("gcp: build http client: {e}")).caused_by(e))?;
    Ok(crate::transport::client(
        base,
        token_source(ts),
        AuthContext::anonymous(),
    ))
}

/// The resource and acting user for a credential retrieval.
#[derive(Clone, Default, PartialEq, Eq)]
pub struct Request {
    /// The connector or auth-provider resource name. A name matching
    /// `projects/*/locations/*/connectors/*` goes to the IAM Connector service; anything else
    /// (e.g. `…/authProviders/*`) to Agent Identity. Validated before anything is sent: letters,
    /// digits and `._~/-:`, and no empty, `.` or `..` segment.
    pub resource: String,
    /// The acting end user. Required.
    pub user_id: String,
    /// The OAuth scopes requested.
    pub scopes: Vec<String>,
    /// Where the consent flow returns to (3-legged flows); unused by non-interactive ones.
    pub continue_uri: String,
    /// The rejected token; non-empty asks for a forced refresh. Agent Identity is sent the token,
    /// the IAM Connector a boolean.
    pub prior_token: String,
}

impl fmt::Debug for Request {
    // prior_token "<redacted>".
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Request")
            .field("resource", &self.resource)
            .field("user_id", &self.user_id)
            .field("scopes", &self.scopes)
            .field("continue_uri", &self.continue_uri)
            .field(
                "prior_token",
                &if self.prior_token.is_empty() {
                    ""
                } else {
                    "<redacted>"
                },
            )
            .finish()
    }
}

/// A retrieved credential. `expires_at: None` means the lifetime is unknown: the service reports
/// none when the token may be permanent or cannot say, and an unparseable one arrives the same way.
/// Never read it as "expires now", and never cache on it.
#[derive(Clone, Debug)]
pub struct Retrieval {
    /// Authenticates outbound requests as the end user.
    pub credential: Arc<dyn crate::Credential>,
    /// When it expires, if the service said.
    pub expires_at: Option<DateTime<Utc>>,
}

pub(crate) const CLOUD_PLATFORM_SCOPE: &str = "https://www.googleapis.com/auth/cloud-platform";
pub(crate) const DEFAULT_AGENT_IDENTITY_URL: &str =
    "https://agentidentitycredentials.googleapis.com";
pub(crate) const DEFAULT_CONNECTOR_URL: &str = "https://iamconnectorcredentials.googleapis.com";
pub(crate) const DEFAULT_POLL_TIMEOUT: Duration = Duration::from_secs(10);
/// The service documents an exponential polling backoff (0.5, 1, 2, 4, 8 s); these track it.
pub(crate) const DEFAULT_INITIAL_BACKOFF: Duration = Duration::from_millis(500);
pub(crate) const MAX_BACKOFF: Duration = Duration::from_secs(8);
/// The cap on a response body read by `do_post`.
pub(crate) const MAX_BODY: usize = 1 << 20;
/// The width of `CLIENT_NONCE`: 128 bits makes a collision between two processes not worth
/// reasoning about.
pub(crate) const NONCE_BYTES: usize = 16;

/// An IAM Connector resource name; anything else is routed to Agent Identity (the same split as
/// adk-python).
#[allow(clippy::expect_used)] // constant pattern, cannot fail
pub(crate) static CONNECTOR_RESOURCE_RE: LazyLock<regex::Regex> = LazyLock::new(|| {
    regex::Regex::new(r"^projects/[^/]+/locations/[^/]+/connectors/[^/]+$")
        .expect("valid connector resource regex")
});
/// With `CONNECTOR_RESOURCE_RE`, the full set
/// `new_provider` accepts. The client itself routes any non-connector name to Agent Identity.
#[allow(clippy::expect_used)] // constant pattern, cannot fail
pub(crate) static AUTH_PROVIDER_RESOURCE_RE: LazyLock<regex::Regex> = LazyLock::new(|| {
    regex::Regex::new(r"^projects/[^/]+/locations/[^/]+/authProviders/[^/]+$")
        .expect("valid auth provider resource regex")
});
/// The characters GCP resource names use. It cannot inject a query, a fragment, an authority or a
/// percent-escape into the URL the name is interpolated into; the colon allows domain-scoped
/// project ids and cannot start a scheme there, because the name always follows the endpoint and a
/// version segment.
#[allow(clippy::expect_used)] // constant pattern, cannot fail
pub(crate) static RESOURCE_NAME_RE: LazyLock<regex::Regex> = LazyLock::new(|| {
    regex::Regex::new(r"^[A-Za-z0-9._~:/-]+$").expect("valid resource name regex")
});
/// This process's client nonce (32 hex chars from `uuid::Uuid::new_v4().into_bytes()`): separates
/// one process's client numbering from another's.
pub(crate) static CLIENT_NONCE: LazyLock<String> = LazyLock::new(new_client_nonce);

/// `NONCE_BYTES` random bytes in lowercase hex. The bytes come from a v4 UUID, whose version and
/// variant bits are fixed, so 122 of the 128 bits are random; it is not a wire surface.
pub(crate) fn new_client_nonce() -> String {
    let bytes: [u8; NONCE_BYTES] = uuid::Uuid::new_v4().into_bytes();
    hex_encode(&bytes)
}

/// Numbers clients within this process.
pub(crate) static CLIENT_SEQ: AtomicU64 = AtomicU64::new(0);

/// Validates a resource name: the errors are `resource "<name>" has invalid characters` and
/// `resource "<name>" has an empty or relative path segment` (unprefixed; callers prefix). The
/// segment rule keeps routing (matched on the name as given) and path normalization from
/// disagreeing about which resource a name is.
pub(crate) fn validate_resource(name: &str) -> Result<(), Error> {
    if !RESOURCE_NAME_RE.is_match(name) {
        return Err(Error::new(format!(
            "resource {} has invalid characters",
            go_quote(name)
        )));
    }
    if name
        .split('/')
        .any(|seg| seg.is_empty() || seg == "." || seg == "..")
    {
        return Err(Error::new(format!(
            "resource {} has an empty or relative path segment",
            go_quote(name)
        )));
    }
    Ok(())
}

/// Joins fields as "<len>:<field>" per field (byte lengths), so no delimiter inside a field can
/// make two different field lists encode alike.
pub(crate) fn join_fields(fields: &[&str]) -> String {
    let mut out = String::new();
    for field in fields {
        out.push_str(&field.len().to_string());
        out.push(':');
        out.push_str(field);
    }
    out
}

/// The cache slot of a scheme: hex SHA-256 of
/// `join_fields([client.cache_slot, name, scopes.len(), sorted scopes…, continue_uri])`. Lives here
/// because `cache_key` calls it. Scopes are sorted (byte order) but not deduplicated.
pub(crate) fn cache_slot(client: &Client, scheme: &ProviderScheme) -> String {
    let mut scopes: Vec<&str> = scheme.scopes.iter().map(String::as_str).collect();
    scopes.sort_unstable();
    let count = scopes.len().to_string();
    let mut fields = vec![
        client.cache_slot.as_str(),
        scheme.name.as_str(),
        count.as_str(),
    ];
    fields.extend(scopes);
    fields.push(scheme.continue_uri.as_str());
    let sum = Sha256::digest(join_fields(&fields).as_bytes());
    hex_encode(&sum)
}

/// Maps a retrieved header and token to a credential: "Authorization: Bearer" becomes a [`Bearer`];
/// any other header name becomes a header [`ApiKey`] mirrored into `X-Goog-Api-Key` (adk-python
/// parity). `secrets` are scrubbed from the rejection of an unusable name, which is service text.
pub(crate) fn map_credential(
    header: &str,
    token: &str,
    secrets: &[&str],
) -> Result<Arc<dyn Credential>, Error> {
    if header.is_empty() || token.is_empty() {
        return Err(Error::new(
            "gcp: credentials service returned an empty header or token",
        ));
    }
    // Split on the first ":". The comparison with "authorization" is ASCII case-folding: none of
    // its letters has a non-ASCII simple fold (unlike k and s). `str::trim` strips the Unicode
    // White_Space set.
    let (name, scheme) = header.split_once(':').unwrap_or((header, ""));
    if name.trim().eq_ignore_ascii_case("authorization")
        && go_to_lower(scheme.trim()).starts_with("bearer")
    {
        return Ok(Arc::new(Bearer {
            token: token.to_string(),
        }));
    }
    // Rejecting an unusable name here keeps the failure at its cause, rather than at the
    // eventual request the credential is applied to.
    if !valid_header_field_name(header) {
        return Err(Error::new(format!(
            "gcp: credentials service returned {}, which is not a usable HTTP header name",
            go_quote(&redacted_for_error(header, secrets))
        )));
    }
    let key: Arc<dyn Credential> = Arc::new(ApiKey {
        name: header.to_string(),
        value: token.to_string(),
    });
    Ok(with_headers(
        key,
        BTreeMap::from([("X-Goog-Api-Key".to_string(), token.to_string())]),
    ))
}

/// Whether `s` is an RFC 9110 field name (a token).
pub(crate) fn valid_header_field_name(s: &str) -> bool {
    !s.is_empty()
        && s.bytes()
            .all(|c| c.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&c))
}

/// Parses an expiry time (RFC 3339; empty or unparseable → None) by the rules of
/// [`go_parse_rfc3339`].
pub(crate) fn parse_expire_time(v: &str) -> Option<DateTime<Utc>> {
    if v.is_empty() {
        return None;
    }
    go_parse_rfc3339(v)
}

/// JSON string escaping on top of serde_json's: `<`, `>`, `&`, U+2028 and U+2029 are written as
/// `\u003c`, `\u003e`, `\u0026`, `\u2028` and `\u2029`, so request bodies match those of ADK
/// clients byte for byte. serde_json already escapes `"`, `\` and control characters
/// (`\b \f \n \r \t`, else lowercase `\u00XX`), and calls this only for the runs between those.
struct GoJsonFormatter;

impl serde_json::ser::Formatter for GoJsonFormatter {
    fn write_string_fragment<W>(&mut self, writer: &mut W, fragment: &str) -> std::io::Result<()>
    where
        W: ?Sized + std::io::Write,
    {
        let mut start = 0;
        for (i, c) in fragment.char_indices() {
            let escaped = match c {
                '<' => "\\u003c",
                '>' => "\\u003e",
                '&' => "\\u0026",
                '\u{2028}' => "\\u2028",
                '\u{2029}' => "\\u2029",
                _ => continue,
            };
            writer.write_all(&fragment.as_bytes()[start..i])?;
            writer.write_all(escaped.as_bytes())?;
            start = i + c.len_utf8();
        }
        writer.write_all(&fragment.as_bytes()[start..])
    }
}

/// Serializes `body` as compact JSON, struct fields in declaration order, with the escaping of
/// [`GoJsonFormatter`].
fn go_marshal<B: serde::Serialize + ?Sized>(body: &B) -> Result<Vec<u8>, serde_json::Error> {
    let mut buf = Vec::new();
    let mut ser = serde_json::Serializer::with_formatter(&mut buf, GoJsonFormatter);
    body.serialize(&mut ser)?;
    Ok(buf)
}

/// The result of one retrieval request.
#[derive(Clone, PartialEq, Eq)]
pub(crate) enum Outcome {
    /// A retrieved {header, token} credential and its expiry (`None` when not reported).
    Cred {
        header: String,
        token: String,
        expires_at: Option<DateTime<Utc>>,
    },
    /// Still pending; poll again.
    Pending,
    /// Interactive consent is required at `auth_uri`.
    Consent { auth_uri: String, nonce: String },
    /// The end user rejected consent.
    Rejected,
}

impl fmt::Debug for Outcome {
    // The token is "<redacted>".
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Outcome::Cred {
                header,
                token,
                expires_at,
            } => f
                .debug_struct("Cred")
                .field("header", header)
                .field("token", &if token.is_empty() { "" } else { "<redacted>" })
                .field("expires_at", expires_at)
                .finish(),
            Outcome::Pending => f.write_str("Pending"),
            Outcome::Consent { auth_uri, nonce } => f
                .debug_struct("Consent")
                .field("auth_uri", auth_uri)
                .field("nonce", nonce)
                .finish(),
            Outcome::Rejected => f.write_str("Rejected"),
        }
    }
}

/// The success shape both services share, under
/// "success" for Agent Identity and "response" for the connector operation. An empty
/// `expire_time` means the service cannot say when the token expires (possibly never).
#[derive(Default, serde::Deserialize)]
pub(crate) struct CredentialPayload {
    #[serde(default, deserialize_with = "go_zero")]
    pub(crate) token: String,
    #[serde(default, deserialize_with = "go_zero")]
    pub(crate) header: String,
    /// Only the cache reads it and the connector's shape for it is an assumption, so a value of an
    /// unexpected type costs a cache entry, never the retrieval.
    #[serde(default, rename = "expireTime", deserialize_with = "lenient_time")]
    pub(crate) expire_time: String,
}

/// An object that is present (any JSON object) or absent.
#[derive(Debug, Default, serde::Deserialize)]
pub(crate) struct Empty {}

/// Decodes a struct from JSON: `null` leaves it zero (default), anything but an object fails, and a
/// repeated key keeps its last value.
pub(crate) struct GoObject<T>(pub(crate) T);

impl<'de, T> serde::Deserialize<'de> for GoObject<T>
where
    T: serde::de::DeserializeOwned + Default,
{
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        Ok(GoObject(go_object(d)?.unwrap_or_default()))
    }
}

/// An optional struct field: `null` is `None`, and only a JSON object decodes. It goes through
/// `serde_json::Map` because serde's derive would also accept an array as a struct, and the map
/// insert gives last-key-wins.
pub(crate) fn go_object<'de, D, T>(d: D) -> Result<Option<T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: serde::de::DeserializeOwned,
{
    match <Option<serde_json::Map<String, serde_json::Value>> as serde::Deserialize>::deserialize(
        d,
    )? {
        None => Ok(None),
        Some(map) => serde_json::from_value(serde_json::Value::Object(map))
            .map(Some)
            .map_err(<D::Error as serde::de::Error>::custom),
    }
}

/// A value field (string, bool, integer): `null` leaves the default value.
pub(crate) fn go_zero<'de, D, T>(d: D) -> Result<T, D::Error>
where
    D: serde::Deserializer<'de>,
    T: serde::Deserialize<'de> + Default,
{
    Ok(<Option<T> as serde::Deserialize>::deserialize(d)?.unwrap_or_default())
}

/// A lenient time field: a JSON string is kept, any other value is "".
fn lenient_time<'de, D: serde::Deserializer<'de>>(d: D) -> Result<String, D::Error> {
    Ok(
        match <serde_json::Value as serde::Deserialize>::deserialize(d)? {
            serde_json::Value::String(s) => s,
            _ => String::new(),
        },
    )
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    fn offline_client() -> Client {
        Client::new(Config {
            http_client: Some(reqwest::Client::new().into()),
            ..Default::default()
        })
        .unwrap()
    }

    /// `parse_expire_time` is Go's `time.Parse(time.RFC3339, v)` (gadk-platform's
    /// `go_parse_rfc3339`, whose tests hold the full Go table) with an empty value as `None`.
    #[test]
    fn parse_expire_time_matches_go_time_parse() {
        let cases: &[(&str, Option<&str>)] = &[
            ("2999-01-01T00:00:00Z", Some("2999-01-01T00:00:00Z")),
            (
                "2999-01-01T00:00:00.123+05:30",
                Some("2998-12-31T18:30:00.123Z"),
            ),
            ("2999-01-01t00:00:00Z", None),
            ("", None),
        ];
        for (input, want) in cases {
            let want = want.map(|w| DateTime::parse_from_rfc3339(w).unwrap().with_timezone(&Utc));
            assert_eq!(
                parse_expire_time(input),
                want,
                "parse_expire_time({input:?})"
            );
        }
    }

    #[test]
    fn client_nonce_is_128_bits_of_lowercase_hex_and_names_every_slot() {
        assert_eq!(CLIENT_NONCE.len(), 2 * NONCE_BYTES);
        assert!(
            CLIENT_NONCE
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)),
            "{}",
            *CLIENT_NONCE
        );
        assert_ne!(*CLIENT_NONCE, new_client_nonce(), "the nonce is fixed");
        let (a, b) = (offline_client(), offline_client());
        assert!(
            a.cache_slot.starts_with(&format!("32:{}", *CLIENT_NONCE)),
            "{}",
            a.cache_slot
        );
        assert_ne!(a.cache_slot, b.cache_slot, "two clients share a cache slot");
    }

    /// The expected keys are Go's `cacheSlot` output for the same inputs (sha256 of
    /// `joinFields`, hex), so a key a store persisted under Go reads the same.
    #[test]
    fn cache_slot_and_cache_key_match_go() {
        assert_eq!(join_fields(&["abcdef0123", "42"]), "10:abcdef01232:42");
        let mut c = offline_client();
        c.cache_slot = "SLOT".to_string();
        let scheme = ProviderScheme {
            name: "projects/p/locations/l/connectors/co".into(),
            scopes: vec![
                "https://www.googleapis.com/auth/drive".into(),
                "b".into(),
                "a".into(),
                "é".into(),
            ],
            continue_uri: "https://app.test/cb".into(),
        };
        let want = "74cc71a7d38f8470c9aeb652bd15c43cfb6dce99bfcf0fad5d9ef1d94e7b69cf";
        assert_eq!(cache_slot(&c, &scheme), want);
        let bare = ProviderScheme {
            name: "projects/p/locations/l/authProviders/ap".into(),
            ..Default::default()
        };
        assert_eq!(
            cache_slot(&c, &bare),
            "b9d2680dbe02dce08f79ef26b4323665a2c4f08b5af7bbc74c7c29520f2695fb"
        );
        let id = gadk_agent::Identity {
            user_id: "user-1".into(),
            app_name: "app".into(),
            session_id: "s-1".into(),
        };
        assert_eq!(
            c.cache_key(&scheme, &id),
            crate::CredentialKey {
                app_name: "app".into(),
                user_id: "user-1".into(),
                key: want.into(),
            }
        );
    }

    #[test]
    fn validate_resource_and_map_credential_texts_match_go() {
        assert_eq!(
            validate_resource("a b").unwrap_err().to_string(),
            r#"resource "a b" has invalid characters"#
        );
        assert_eq!(
            validate_resource("a//b").unwrap_err().to_string(),
            r#"resource "a//b" has an empty or relative path segment"#
        );
        assert!(validate_resource("projects/example.com:p/locations/l/authProviders/a..b").is_ok());
        assert_eq!(
            map_credential("", "t", &[]).err().unwrap().to_string(),
            "gcp: credentials service returned an empty header or token"
        );
        // Go: mapCredential("X-User-alice@example.test-Token", "t", "alice@example.test", "", "").
        assert_eq!(
            map_credential(
                "X-User-alice@example.test-Token",
                "t",
                &["alice@example.test", "", ""]
            )
            .err()
            .unwrap()
            .to_string(),
            r#"gcp: credentials service returned "x-user-[redacted]-token", which is not a usable HTTP header name"#
        );
        assert!(valid_header_field_name("X-Goog-Api-Key"));
        assert!(!valid_header_field_name("X-Api-Key: Token"));
        assert!(!valid_header_field_name(""));
    }

    #[test]
    fn debug_withholds_the_prior_token() {
        let req = Request {
            resource: "projects/p/locations/l/authProviders/ap".into(),
            user_id: "u".into(),
            prior_token: "prior-secret".into(),
            ..Default::default()
        };
        let s = format!("{req:?}");
        assert!(!s.contains("prior-secret"), "{s}");
        assert!(s.contains(r#"prior_token: "<redacted>""#), "{s}");
        assert!(format!("{:?}", Request::default()).contains(r#"prior_token: """#));
        let cfg = Config {
            agent_identity_endpoint: "https://ai.example.com".into(),
            ..Default::default()
        };
        let s = format!("{cfg:?}");
        assert!(
            s.contains("https://ai.example.com") && s.contains("http_client: None"),
            "{s}"
        );
        let s = format!("{:?}", offline_client());
        assert!(
            s.starts_with("Client {") && s.contains(DEFAULT_CONNECTOR_URL),
            "{s}"
        );
        let cred = Outcome::Cred {
            header: "h".into(),
            token: "tok-secret".into(),
            expires_at: None,
        };
        assert!(!format!("{cred:?}").contains("tok-secret"));
    }

    /// The ADC failure reads as Go's "gcp: find default credentials: <builder error>", not with
    /// the auth seam's own prefix doubled in, and keeps the seam's error as its source.
    #[tokio::test]
    async fn with_adc_reports_a_discovery_failure_in_gos_words() {
        // An authorized_user key missing its client fields fails google-cloud-auth's builder.
        let adc = AdcSource::Json(serde_json::json!({ "type": "authorized_user" }));
        let err = Client::with_adc(Config::default(), &adc).unwrap_err();
        let msg = err.to_string();
        assert!(msg.starts_with("gcp: find default credentials: "), "{msg}");
        assert!(!msg.contains("auth: find default credentials"), "{msg}");
        assert_eq!(err.kind(), None);
        let source = std::error::Error::source(&err).expect("the seam's error is the source");
        assert!(
            matches!(
                source.downcast_ref::<crate::Error>(),
                Some(crate::Error::FindDefaultCredentials(_))
            ),
            "{source:?}"
        );
    }

    mod wire {
        use parking_lot::Mutex;

        use super::super::*;
        use crate::error::find_consent;
        use crate::gcp::is_kind;

        const AP: &str = "projects/p/locations/l/authProviders/ap";
        const CO: &str = "projects/p/locations/l/connectors/co";

        struct Call {
            at: Instant,
            method: String,
            url: String,
            content_type: Option<String>,
            accept: Option<String>,
            body: String,
        }

        struct Stub {
            bodies: Vec<String>,
            calls: Arc<Mutex<Vec<Call>>>,
        }

        #[async_trait::async_trait]
        impl reqwest_middleware::Middleware for Stub {
            async fn handle(
                &self,
                req: reqwest::Request,
                _extensions: &mut http::Extensions,
                _next: reqwest_middleware::Next<'_>,
            ) -> reqwest_middleware::Result<reqwest::Response> {
                let header = |name: http::header::HeaderName| {
                    req.headers()
                        .get(name)
                        .and_then(|v| v.to_str().ok())
                        .map(str::to_string)
                };
                let call = Call {
                    at: Instant::now(),
                    method: req.method().to_string(),
                    url: req.url().to_string(),
                    content_type: header(http::header::CONTENT_TYPE),
                    accept: header(http::header::ACCEPT),
                    body: String::from_utf8_lossy(
                        req.body()
                            .and_then(reqwest::Body::as_bytes)
                            .unwrap_or_default(),
                    )
                    .into_owned(),
                };
                let reply = {
                    let mut calls = self.calls.lock();
                    calls.push(call);
                    self.bodies[(calls.len() - 1).min(self.bodies.len() - 1)].clone()
                };
                Ok(reqwest::Response::from(http::Response::new(
                    reqwest::Body::from(reply),
                )))
            }
        }

        /// A client whose every call is answered by `bodies` in order (the last repeats).
        fn stub_client(bodies: &[&str], poll_timeout: Duration) -> (Client, Arc<Mutex<Vec<Call>>>) {
            let calls = Arc::new(Mutex::new(Vec::new()));
            let stub = Stub {
                bodies: bodies.iter().map(|b| (*b).to_string()).collect(),
                calls: calls.clone(),
            };
            let http = reqwest_middleware::ClientBuilder::new(reqwest::Client::new())
                .with(stub)
                .build();
            let c = Client::new(Config {
                http_client: Some(http),
                agent_identity_endpoint: "https://ai.stub.test".into(),
                connector_endpoint: "https://conn.stub.test".into(),
                poll_timeout,
            })
            .unwrap();
            (c, calls)
        }

        fn request(resource: &str, user_id: &str) -> Request {
            Request {
                resource: resource.into(),
                user_id: user_id.into(),
                ..Default::default()
            }
        }

        /// The body is `encoding/json`'s output for the same `agentIdentityRequest`, captured
        /// from Go: HTML-safe escapes, the `\b`/`\f` short forms, lowercase `\u00XX`.
        #[tokio::test]
        async fn agent_identity_request_is_gos_json_bytes() {
            let ok = r#"{"success":{"token":"t","header":"Authorization: Bearer"}}"#;
            let (c, calls) = stub_client(&[ok], Duration::ZERO);
            c.retrieve_credential(Request {
                resource: AP.into(),
                user_id: "a<b>&c\u{2028}\u{2029}\u{8}\u{c}\u{1}\u{1f}\"\\/é".into(),
                scopes: vec!["s1".into(), "s2".into()],
                continue_uri: "https://x.test/cb?a=1&b=2".into(),
                prior_token: "tok".into(),
            })
            .await
            .unwrap();
            let calls = calls.lock();
            assert_eq!(calls.len(), 1);
            let call = &calls[0];
            assert_eq!(call.method, "POST");
            assert_eq!(
                call.url,
                "https://ai.stub.test/v1/projects/p/locations/l/authProviders/ap/credentials:retrieve"
            );
            assert_eq!(call.content_type.as_deref(), Some("application/json"));
            assert_eq!(call.accept.as_deref(), Some("application/json"));
            assert_eq!(
                call.body,
                r#"{"userId":"a\u003cb\u003e\u0026c\u2028\u2029\b\f\u0001\u001f\"\\/é","scopes":["s1","s2"],"continueUri":"https://x.test/cb?a=1\u0026b=2","forceRefreshToken":"tok"}"#
            );
        }

        /// The connector is sent a boolean where Agent Identity is sent the token, and both
        /// omit empty fields (Go `omitempty`); the bodies are Go's.
        #[tokio::test]
        async fn connector_request_sends_force_refresh_as_a_boolean() {
            let done = r#"{"done":true,"response":{"token":"t","header":"Authorization: Bearer"}}"#;
            let (c, calls) = stub_client(&[done], Duration::ZERO);
            c.retrieve_credential(Request {
                resource: CO.into(),
                user_id: "u".into(),
                scopes: vec!["s".into()],
                continue_uri: "c".into(),
                prior_token: "tok".into(),
            })
            .await
            .unwrap();
            c.retrieve_credential(request(CO, "u")).await.unwrap();
            {
                // Scoped: the guard must be gone before the next await.
                let calls = calls.lock();
                assert_eq!(
                    calls[0].url,
                    "https://conn.stub.test/v1alpha/projects/p/locations/l/connectors/co/credentials:retrieve"
                );
                assert_eq!(
                    calls[0].body,
                    r#"{"userId":"u","scopes":["s"],"continueUri":"c","forceRefresh":true}"#
                );
                assert_eq!(calls[1].body, r#"{"userId":"u"}"#);
            }

            let ok = r#"{"success":{"token":"t","header":"Authorization: Bearer"}}"#;
            let (c, calls) = stub_client(&[ok], Duration::ZERO);
            c.retrieve_credential(request(AP, "u")).await.unwrap();
            assert_eq!(calls.lock()[0].body, r#"{"userId":"u"}"#);
        }

        /// R8: 500 ms initial backoff doubling to 8 s, capped by the poll timeout (default
        /// 10 s). The last sleep is cut to what remains, and one more call is made at the
        /// deadline before PollTimeout.
        #[tokio::test(start_paused = true)]
        async fn pending_backs_off_from_500ms_doubling_to_8s_within_the_poll_timeout() {
            let cases: [(Duration, &[u64]); 2] = [
                // Zero is the 10 s default.
                (Duration::ZERO, &[0, 500, 1500, 3500, 7500, 10_000]),
                (
                    Duration::from_secs(40),
                    &[
                        0, 500, 1500, 3500, 7500, 15_500, 23_500, 31_500, 39_500, 40_000,
                    ],
                ),
            ];
            for (poll_timeout, want_ms) in cases {
                let (c, calls) = stub_client(&[r#"{"pending":{}}"#], poll_timeout);
                let start = Instant::now();
                let err = c.retrieve_credential(request(AP, "u")).await.unwrap_err();
                assert!(is_kind(&err, ErrorKind::PollTimeout), "{err}");
                assert_eq!(
                    err.to_string(),
                    r#"gcp: timed out waiting for credentials (resource "projects/p/locations/l/authProviders/ap")"#
                );
                let got: Vec<u64> = calls
                    .lock()
                    .iter()
                    .map(|call| u64::try_from((call.at - start).as_millis()).unwrap())
                    .collect();
                assert_eq!(got, want_ms, "call times for poll_timeout {poll_timeout:?}");
            }

            // Pending, then a credential on the next poll, 500 ms later.
            let ok = r#"{"success":{"token":"t","header":"Authorization: Bearer"}}"#;
            let (c, calls) = stub_client(&[r#"{"pending":{}}"#, ok], Duration::ZERO);
            let start = Instant::now();
            c.retrieve_credential(request(AP, "u")).await.unwrap();
            let got: Vec<Duration> = calls.lock().iter().map(|call| call.at - start).collect();
            assert_eq!(got, [Duration::ZERO, Duration::from_millis(500)]);
        }

        /// Consent and rejection carry Go's texts, decorated with the resource, and stay
        /// matchable through the decoration.
        #[tokio::test]
        async fn consent_and_rejection_name_the_resource() {
            let consent = r#"{"uriConsentRequired":{"authorizationUri":"https://consent.test/a?b=c","consentNonce":"n-1"}}"#;
            let (c, _) = stub_client(&[consent], Duration::ZERO);
            let err = c.retrieve_credential(request(AP, "u")).await.unwrap_err();
            assert_eq!(
                err.to_string(),
                r#"auth: interactive consent required (resource "projects/p/locations/l/authProviders/ap")"#
            );
            assert_eq!(err.kind(), None);
            assert_eq!(
                find_consent(&err),
                Some(&ConsentRequiredError {
                    auth_uri: "https://consent.test/a?b=c".into(),
                    nonce: "n-1".into(),
                    key: String::new(),
                })
            );

            let (c, _) = stub_client(&[r#"{"metadata":{"consentRejected":{}}}"#], Duration::ZERO);
            let err = c.retrieve_credential(request(CO, "u")).await.unwrap_err();
            assert_eq!(
                err.to_string(),
                r#"gcp: user consent rejected (resource "projects/p/locations/l/connectors/co")"#
            );
            assert_eq!(err.kind(), Some(ErrorKind::ConsentRejected));
        }
    }
}
