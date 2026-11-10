// SPDX-License-Identifier: Apache-2.0
//! A6: supervised children get an allow-listed environment, not the daemon's.
//! One test fn in its own binary because it mutates the process environment.
#![allow(clippy::unwrap_used)]

use origin_tools::builtins::bash::{bash_v2, BashArgs};
use origin_tools::proc_supervisor::Supervisor;

async fn child_sees() -> String {
    #[cfg(unix)]
    let cmd = r#"echo "K=$ANTHROPIC_API_KEY"; echo "P=${PATH:+True}"; echo "T=$A6_PROBE_TOKEN""#;
    #[cfg(windows)]
    let cmd = "'K=' + $env:ANTHROPIC_API_KEY; 'P=' + ($env:PATH.Length -gt 0); 'T=' + $env:A6_PROBE_TOKEN";
    let args = BashArgs {
        command: cmd.into(),
        timeout: Some(60),
        cwd: None,
        env: vec![],
        run_in_background: false,
    };
    let out = bash_v2(args, &Supervisor::new()).await.unwrap();
    assert_eq!(out["exit_code"], 0, "{out}");
    out["stdout"].as_str().unwrap().replace('\r', "")
}

#[tokio::test]
async fn child_env_drops_secrets_keeps_path_and_passthrough() {
    std::env::set_var("ANTHROPIC_API_KEY", "sk-parent-secret");
    std::env::set_var("A6_PROBE_TOKEN", "probe-visible");
    std::env::set_var("ORIGIN_ENV_PASSTHROUGH", "OTHER_NAME, A6_PROBE_TOKEN");
    std::env::remove_var("ORIGIN_ENV_SCRUB");

    let out = child_sees().await;
    assert!(out.lines().any(|l| l == "K="), "secret leaked to child: {out:?}");
    assert!(
        out.lines().any(|l| l == "P=True"),
        "PATH missing in child: {out:?}"
    );
    assert!(
        out.lines().any(|l| l == "T=probe-visible"),
        "passthrough name missing: {out:?}"
    );

    // Escape hatch: ORIGIN_ENV_SCRUB=0 restores full inheritance.
    std::env::set_var("ORIGIN_ENV_SCRUB", "0");
    let out = child_sees().await;
    assert!(
        out.lines().any(|l| l == "K=sk-parent-secret"),
        "scrub not disabled: {out:?}"
    );
}
