// Copyright 2026 Chris Olson
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

//! End-to-end tests of the `gadk-prompts` command-line tool.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

struct Out {
    code: i32,
    stdout: String,
    stderr: String,
}

async fn cli(dir: &std::path::Path, args: &[&str], stdin: &str) -> Out {
    let mut argv = vec![
        "gadk-prompts".to_owned(),
        "--store".to_owned(),
        format!("file://{}", dir.display()),
    ];
    argv.extend(args.iter().map(|s| (*s).to_owned()));
    let (mut o, mut e) = (Vec::new(), Vec::new());
    let code = gadk_prompts_cli::run(argv, &mut stdin.as_bytes(), &mut o, &mut e).await;
    Out {
        code,
        stdout: String::from_utf8(o).unwrap(),
        stderr: String::from_utf8(e).unwrap(),
    }
}

#[tokio::test]
async fn cli_end_to_end() {
    let d = tempfile::tempdir().unwrap();
    let p = d.path();

    let r = cli(
        p,
        &["push", "p", "--author", "me", "-m", "first"],
        "hello {who}",
    )
    .await;
    assert_eq!((r.code, r.stdout.as_str()), (0, "p v1\n"), "{}", r.stderr);
    let r = cli(
        p,
        &["push", "p", "--template", "-", "--author", "me"],
        "hello {who}",
    )
    .await;
    assert_eq!((r.code, r.stdout.as_str()), (0, "p v1 (unchanged)\n"));

    let tpl = p.join("t.txt");
    std::fs::write(&tpl, "hi {who}\n").unwrap();
    let cfg = p.join("c.json");
    std::fs::write(&cfg, r#"{"temperature":0.2}"#).unwrap();
    let r = cli(
        p,
        &[
            "push",
            "p",
            "--template",
            tpl.to_str().unwrap(),
            "--config",
            cfg.to_str().unwrap(),
            "--author",
            "me",
        ],
        "",
    )
    .await;
    assert_eq!((r.code, r.stdout.as_str()), (0, "p v2\n"), "{}", r.stderr);

    let r = cli(
        p,
        &[
            "promote",
            "p",
            "1",
            "--label",
            "production",
            "--expect",
            "none",
        ],
        "",
    )
    .await;
    assert_eq!(
        (r.code, r.stdout.as_str()),
        (0, "p production -> v1\n"),
        "{}",
        r.stderr
    );
    let r = cli(
        p,
        &[
            "promote",
            "p",
            "2",
            "--label",
            "production",
            "--expect",
            "2",
        ],
        "",
    )
    .await;
    assert_eq!(r.code, 3, "{} / {}", r.stdout, r.stderr);
    assert!(r.stderr.starts_with("gadk-prompts: "), "{}", r.stderr);
    let r = cli(p, &["promote", "p", "2", "--label", "staging"], "").await;
    assert_eq!(r.code, 0);

    let r = cli(p, &["list"], "").await;
    assert_eq!(r.stdout, "p\tv2\tproduction=v1,staging=v2\n");
    let r = cli(p, &["list", "--prefix", "zz"], "").await;
    assert_eq!((r.code, r.stdout.as_str()), (0, ""));

    let r = cli(p, &["show", "p", "--label", "production"], "").await;
    let mut lines = r.stdout.lines();
    let header: Vec<&str> = lines.next().unwrap().split(' ').collect();
    assert_eq!(
        (header[0], header[1], header[2].len(), header[4]),
        ("p", "v1", 64, "me")
    );
    assert_eq!(lines.next(), Some(""));
    assert_eq!(lines.collect::<Vec<_>>().join("\n"), "hello {who}");
    let r = cli(p, &["show", "p", "--json"], "").await;
    let v: serde_json::Value = serde_json::from_str(r.stdout.trim()).unwrap();
    assert_eq!(v["version"], 2);
    assert_eq!(v["config"]["temperature"], 0.2);

    let r = cli(p, &["diff", "p", "v1", "production"], "").await;
    assert_eq!((r.code, r.stdout.as_str()), (0, ""));
    let r = cli(p, &["diff", "p", "v1", "latest"], "").await;
    assert_eq!(r.code, 0);
    assert!(
        r.stdout.contains("-hello {who}") && r.stdout.contains("+hi {who}"),
        "{}",
        r.stdout
    );
    assert!(r.stdout.contains("+  \"temperature\": 0.2"), "{}", r.stdout);

    let r = cli(p, &["unlabel", "p", "--label", "staging"], "").await;
    assert_eq!((r.code, r.stdout.as_str()), (0, "p staging removed\n"));

    let r = cli(p, &["show", "nope"], "").await;
    assert_eq!(r.code, 2);
    let r = cli(p, &["promote", "p", "9", "--label", "production"], "").await;
    assert_eq!(r.code, 2);
    let r = cli(p, &["promote", "p", "vvv1", "--label", "production"], "").await;
    assert_eq!(r.code, 1, "{}", r.stderr);
    assert!(r.stderr.contains("bad version"), "{}", r.stderr);
    let r = cli(p, &["promote", "p", "v1", "--label", "canary"], "").await;
    assert_eq!(
        (r.code, r.stdout.as_str()),
        (0, "p canary -> v1\n"),
        "{}",
        r.stderr
    );
    let r = cli(p, &["list", "--bogus"], "").await;
    assert_eq!(r.code, 1);
    let r = cli(p, &["push", "Bad Name"], "x").await;
    assert_eq!(r.code, 1);
}

#[tokio::test]
async fn cli_bad_store_url_is_usage_error() {
    let (mut o, mut e) = (Vec::new(), Vec::new());
    let argv = ["gadk-prompts", "--store", "s3://x", "list"]
        .map(str::to_owned)
        .to_vec();
    assert_eq!(
        gadk_prompts_cli::run(argv, &mut "".as_bytes(), &mut o, &mut e).await,
        1
    );
}

#[tokio::test]
async fn cli_rollout_start_list_show_promote_abort() {
    let d = tempfile::tempdir().unwrap();
    let p = d.path();
    for t in ["one", "two", "three"] {
        assert_eq!(cli(p, &["push", "p", "--author", "me"], t).await.code, 0);
    }
    assert_eq!(
        cli(p, &["promote", "p", "1", "--label", "production"], "")
            .await
            .code,
        0
    );

    let r = cli(
        p,
        &[
            "rollout",
            "p",
            "--label",
            "production",
            "v2",
            "--percent",
            "10",
        ],
        "",
    )
    .await;
    assert_eq!(
        (r.code, r.stdout.as_str()),
        (0, "p production -> v1 (v2 at 10%)\n"),
        "{}",
        r.stderr
    );
    let r = cli(p, &["list"], "").await;
    assert_eq!(r.stdout, "p\tv3\tproduction=v1(v2@10%)\n");

    let r = cli(p, &["show", "p", "--label", "production"], "").await;
    let lines: Vec<&str> = r.stdout.lines().collect();
    assert_eq!(lines.len(), 4, "{}", r.stdout);
    assert!(lines[0].starts_with("p v1 "), "{}", r.stdout);
    assert_eq!(&lines[1..], ["", "one", "rollout: v2 at 10%"]);
    let r = cli(p, &["show", "p", "--label", "production", "--json"], "").await;
    assert_eq!(r.stdout.lines().count(), 1, "{}", r.stdout);
    let v: serde_json::Value = serde_json::from_str(r.stdout.trim()).unwrap();
    assert_eq!(v["version"], 1);
    let r = cli(p, &["show", "p"], "").await;
    assert!(!r.stdout.contains("rollout"), "{}", r.stdout);
    assert!(r.stdout.ends_with("\nthree\n"), "{}", r.stdout);

    // Replacing the rollout keeps the label where it is.
    let r = cli(
        p,
        &[
            "rollout",
            "p",
            "--label",
            "production",
            "3",
            "--percent",
            "50",
        ],
        "",
    )
    .await;
    assert_eq!(
        (r.code, r.stdout.as_str()),
        (0, "p production -> v1 (v3 at 50%)\n")
    );

    let r = cli(
        p,
        &["rollout", "p", "--label", "production", "--promote"],
        "",
    )
    .await;
    assert_eq!(
        (r.code, r.stdout.as_str()),
        (0, "p production -> v3\n"),
        "{}",
        r.stderr
    );
    let r = cli(p, &["list"], "").await;
    assert_eq!(r.stdout, "p\tv3\tproduction=v3\n");
    let r = cli(
        p,
        &["rollout", "p", "--label", "production", "--promote"],
        "",
    )
    .await;
    assert_eq!(
        (r.code, r.stderr.as_str()),
        (2, "gadk-prompts: not found: p@production rollout\n")
    );

    let r = cli(
        p,
        &[
            "rollout",
            "p",
            "--label",
            "production",
            "2",
            "--percent",
            "25",
        ],
        "",
    )
    .await;
    assert_eq!(r.stdout, "p production -> v3 (v2 at 25%)\n");
    let r = cli(p, &["rollout", "p", "--label", "production", "--abort"], "").await;
    assert_eq!(
        (r.code, r.stdout.as_str()),
        (0, "p production rollout aborted\n")
    );
    let r = cli(p, &["list"], "").await;
    assert_eq!(r.stdout, "p\tv3\tproduction=v3\n");
    // Aborting with no rollout is not an error.
    let r = cli(p, &["rollout", "p", "--label", "production", "--abort"], "").await;
    assert_eq!(r.code, 0, "{}", r.stderr);
}

#[tokio::test]
async fn cli_rollout_errors_and_exit_codes() {
    let d = tempfile::tempdir().unwrap();
    let p = d.path();
    for t in ["one", "two"] {
        assert_eq!(cli(p, &["push", "p", "--author", "me"], t).await.code, 0);
    }
    assert_eq!(
        cli(p, &["promote", "p", "1", "--label", "production"], "")
            .await
            .code,
        0
    );
    let cases: &[(&[&str], i32)] = &[
        // exactly one of <version> --percent, --promote, --abort
        (&["rollout", "p", "--label", "production"], 1),
        (
            &[
                "rollout",
                "p",
                "--label",
                "production",
                "--promote",
                "--abort",
            ],
            1,
        ),
        (
            &[
                "rollout",
                "p",
                "--label",
                "production",
                "2",
                "--percent",
                "10",
                "--promote",
            ],
            1,
        ),
        (
            &[
                "rollout",
                "p",
                "--label",
                "production",
                "2",
                "--percent",
                "10",
                "--abort",
            ],
            1,
        ),
        (&["rollout", "p", "--label", "production", "2"], 1),
        (
            &["rollout", "p", "--label", "production", "--percent", "10"],
            1,
        ),
        (
            &["rollout", "p", "--label", "production", "2", "--promote"],
            1,
        ),
        (&["rollout", "p", "2", "--percent", "10"], 1),
        // invalid values
        (
            &[
                "rollout",
                "p",
                "--label",
                "production",
                "2",
                "--percent",
                "0",
            ],
            1,
        ),
        (
            &[
                "rollout",
                "p",
                "--label",
                "production",
                "2",
                "--percent",
                "100",
            ],
            1,
        ),
        (
            &[
                "rollout",
                "p",
                "--label",
                "production",
                "2",
                "--percent",
                "ten",
            ],
            1,
        ),
        (
            &[
                "rollout",
                "p",
                "--label",
                "production",
                "vv2",
                "--percent",
                "10",
            ],
            1,
        ),
        (
            &["rollout", "p", "--label", "Prod", "2", "--percent", "10"],
            1,
        ),
        (
            &[
                "rollout",
                "p",
                "--label",
                "production",
                "1",
                "--percent",
                "10",
            ],
            1,
        ),
        // not found
        (
            &[
                "rollout",
                "p",
                "--label",
                "production",
                "9",
                "--percent",
                "10",
            ],
            2,
        ),
        (
            &["rollout", "p", "--label", "staging", "2", "--percent", "10"],
            2,
        ),
        (
            &[
                "rollout",
                "nope",
                "--label",
                "production",
                "2",
                "--percent",
                "10",
            ],
            2,
        ),
        (
            &["rollout", "nope", "--label", "production", "--promote"],
            2,
        ),
        (&["rollout", "nope", "--label", "production", "--abort"], 2),
        (&["rollout", "p", "--label", "staging", "--abort"], 2),
    ];
    for (args, want) in cases {
        let r = cli(p, args, "").await;
        assert_eq!(r.code, *want, "{args:?}: {} / {}", r.stdout, r.stderr);
        assert_eq!(r.stdout, "", "{args:?}");
        assert!(
            r.stderr.starts_with("gadk-prompts: "),
            "{args:?}: {}",
            r.stderr
        );
    }
    let r = cli(p, &["list"], "").await;
    assert_eq!(r.stdout, "p\tv2\tproduction=v1\n");
}
