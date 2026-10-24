use super::*;
use crate::process::{MAX_ARG_BYTES, MAX_ARGS, MAX_ARGV_BYTES};

fn parse(launch: &str, base: &Path) -> Result<LaunchConfiguration> {
    LaunchConfiguration::parse_toml(
        format!("[minecraft]\nworking-directory = 'game'\n[launch]\n{launch}\n").as_bytes(),
        base,
    )
}
fn refused(launch: &str, base: &Path) {
    let error = parse(launch, base).unwrap_err();
    assert_eq!(error, LaunchConfigError::InvalidConfiguration);
    assert_eq!(error.to_string(), "Launch configuration failed");
    assert!(error.source().is_none());
    assert!(!format!("{error:?}").contains("TOP-SECRET"));
}

#[test]
fn jar_arguments_are_literal_ordered_and_no_implicit_nogui_is_added() {
    let dir = tempfile::tempdir().unwrap();
    let plan = parse("kind = 'java-jar'\njar = 'server.jar'\njvm-args = ['-Xms1G', '-Dsecret=TOP-SECRET']\nargs = ['nogui', 'two words', '', '; echo danger']", dir.path()).unwrap();
    assert_eq!(plan.kind(), LaunchKind::JavaJar);
    assert_eq!(plan.program, None);
    assert_eq!(plan.cwd, dir.path().join("game"));
    assert_eq!(
        plan.required_files,
        [dir.path().join("game").join("server.jar")]
    );
    assert_eq!(
        plan.args,
        [
            "-Xms1G",
            "-Dsecret=TOP-SECRET",
            "-jar",
            dir.path().join("game").join("server.jar").to_str().unwrap(),
            "nogui",
            "two words",
            "",
            "; echo danger"
        ]
    );
    assert_eq!(plan.stop, GracefulStop::MinecraftCommand);
    let minimal = parse("kind = 'java-jar'\njar = 'server.jar'", dir.path()).unwrap();
    assert_eq!(minimal.args.len(), 2);
    assert!(!minimal.args.iter().any(|arg| arg == "nogui"));
}

#[test]
fn argfiles_preserve_order_duplicates_and_explicit_unsupported_stop() {
    let dir = tempfile::tempdir().unwrap();
    let plan = parse("kind = 'java-argfile'\njava-command = 'bin/java'\njvm-args = ['-Dflag=1']\nargfiles = ['user args.txt', 'libraries/args.txt', 'user args.txt']\nargs = ['nogui']\nstop = 'unsupported'", dir.path()).unwrap();
    assert_eq!(plan.kind(), LaunchKind::JavaArgfile);
    let cwd = dir.path().join("game");
    assert_eq!(plan.program, Some(cwd.join("bin/java")));
    assert_eq!(
        plan.args,
        [
            "-Dflag=1".into(),
            format!("@{}", cwd.join("user args.txt").display()),
            format!("@{}", cwd.join("libraries").join("args.txt").display()),
            format!("@{}", cwd.join("user args.txt").display()),
            "nogui".into()
        ]
    );
    assert_eq!(
        plan.required_files,
        [
            cwd.join("user args.txt"),
            cwd.join("libraries").join("args.txt"),
            cwd.join("user args.txt")
        ]
    );
    assert_eq!(plan.stop, GracefulStop::Unsupported);
}

#[test]
fn paths_are_lexically_resolved_without_filesystem_discovery() {
    let dir = tempfile::tempdir().unwrap();
    let absent = dir.path().join("absent");
    let executable = absent.join("elsewhere/../native");
    let plan = parse(
        &format!(
            "kind = 'native'\nexecutable = '{}'\nstop = 'unsupported'",
            executable.display()
        ),
        &absent,
    )
    .unwrap();
    assert_eq!(plan.program, Some(absent.join("native")));
    assert!(!absent.exists());
    let relative = parse(
        "kind = 'native'\nexecutable = '../bin/server'\nstop = 'console-stop'",
        &absent,
    )
    .unwrap();
    assert_eq!(relative.program, Some(absent.join("bin/server")));
    assert_eq!(relative.stop, GracefulStop::MinecraftCommand);
}

#[test]
fn omitted_java_requires_explicit_host_selection_not_a_path_lookup() {
    let dir = tempfile::tempdir().unwrap();
    let plan = parse("kind = 'java-jar'\njar = 'server.jar'", dir.path()).unwrap();
    assert_eq!(
        plan.into_launch_spec(None).unwrap_err(),
        LaunchConfigError::JavaUnavailable
    );
    let plan = parse("kind = 'java-jar'\njar = 'server.jar'", dir.path()).unwrap();
    assert_eq!(
        plan.into_launch_spec(Some(Path::new("java"))).unwrap_err(),
        LaunchConfigError::InvalidConfiguration
    );
}

#[test]
fn malformed_unknown_cross_kind_and_wrong_type_tables_refuse_without_echo() {
    let dir = tempfile::tempdir().unwrap();
    for launch in [
        "",
        "kind = 'docker'",
        "kind = 'JAVA-JAR'",
        "kind = 5",
        "kind = 'java-jar'",
        "kind = 'java-jar'\njar = 5",
        "kind = 'java-jar'\njar = ''",
        "kind = 'java-jar'\njar = ' '",
        "kind = 'java-jar'\njar = 's.jar'\njava-command = ''",
        "kind = 'java-jar'\njar = 's.jar'\njava-command = 5",
        "kind = 'java-jar'\njar = 's.jar'\nargs = [1, 'TOP-SECRET']",
        "kind = 'java-jar'\njar = 's.jar'\njvm-args = 'TOP-SECRET'",
        "kind = 'java-jar'\njar = 's.jar'\nargfiles = []",
        "kind = 'java-jar'\njar = 's.jar'\nexecutable = 'TOP-SECRET'",
        "kind = 'java-jar'\njar = 's.jar'\nshell = 'TOP-SECRET'",
        "kind = 'java-jar'\njar = 's.jar'\nenv = ['TOP-SECRET']",
        "kind = 'java-jar'\njar = 's.jar'\nstop = true",
        "kind = 'java-jar'\njar = 's.jar'\nstop = 'kill'",
        "kind = 'java-jar'\njar = 's.jar'\nkind = 'native'",
        "kind = 'java-argfile'",
        "kind = 'java-argfile'\nargfiles = []",
        "kind = 'java-argfile'\nargfiles = [5]",
        "kind = 'java-argfile'\nargfiles = ['a']\njar = 'TOP-SECRET'",
        "kind = 'native'\nexecutable = 'x'",
        "kind = 'native'\nexecutable = 'x'\nstop = 'unsupported'\njvm-args = 'TOP-SECRET'",
        "kind = 'native'\nexecutable = 'x'\nstop = 'unsupported'\nrequired-files = [5]",
        "kind = 'native'\nexecutable = 'x'\nstop = 'unsupported'\njava-command = 'TOP-SECRET'",
        "kind = 'native'\nexecutable = 'x'\nstop = 'unsupported'\nargfiles = []",
        "kind = 'native'\nexecutable = 'x'\nstop = 'unsupported'\n[launch.extra]\nvalue = 'TOP-SECRET'",
    ] {
        refused(launch, dir.path());
    }
}

#[test]
fn native_launch_preserves_jvm_prefix_and_required_artifacts() {
    let dir = tempfile::tempdir().unwrap();
    let plan = parse(
        "kind = 'native'\nexecutable = 'bin/java'\nstop = 'unsupported'\njvm-args = ['-Xmx1G', '-Dliteral=two words']\nargs = ['Main', 'nogui']\nrequired-files = ['libraries/server.jar']",
        dir.path(),
    )
    .unwrap();
    assert_eq!(plan.kind(), LaunchKind::Native);
    assert_eq!(
        plan.args,
        ["-Xmx1G", "-Dliteral=two words", "Main", "nogui"]
    );
    assert_eq!(
        plan.required_files,
        [dir.path().join("game/libraries/server.jar")]
    );
}

#[test]
fn absent_document_legacy_autostart_security_and_ambiguous_forms_never_fall_back() {
    let dir = tempfile::tempdir().unwrap();
    for text in [
        "",
        "# no launch",
        "launch = 'TOP-SECRET'",
        "[[launch]]\nkind = 'native'",
        "[launch]\nkind = 'native'\nexecutable = 'x'\nstop = 'unsupported'",
        "[minecraft]\nworking-directory = 'game'\nlaunch-jar = 'legacy.jar'",
        "[minecraft]\nworking-directory = 'game'\nauto-start = true\n[launch]\nkind = 'java-jar'\njar = 's.jar'",
        "[minecraft]\nworking-directory = 'game'\njava-command = 'TOP-SECRET'\n[launch]\nkind = 'java-jar'\njar = 's.jar'",
        "[minecraft]\nworking-directory = 'game'\n[launch]\nkind = 'java-jar'\njar = 's.jar'\n[security]\nmode = 'open'",
    ] {
        assert_eq!(
            LaunchConfiguration::parse_toml(text.as_bytes(), dir.path()).unwrap_err(),
            LaunchConfigError::InvalidConfiguration
        );
    }
}

#[test]
fn invalid_utf8_relative_base_and_forbidden_text_are_redacted() {
    let dir = tempfile::tempdir().unwrap();
    assert!(LaunchConfiguration::parse_toml(&[0xff], dir.path()).is_err());
    assert!(parse("kind = 'java-jar'\njar = 's.jar'", Path::new("relative")).is_err());
    for value in [
        "TOP-SECRET\\nstop",
        "TOP-SECRET\\u0000",
        "TOP-SECRET\\u2028",
        "TOP-SECRET\\u2029",
    ] {
        refused(
            &format!("kind = 'java-jar'\njar = 's.jar'\nargs = [\"{value}\"]"),
            dir.path(),
        );
        refused(
            &format!("kind = 'native'\nexecutable = \"{value}\"\nstop = 'unsupported'"),
            dir.path(),
        );
    }
}

#[test]
fn document_byte_bound_is_inclusive() {
    let dir = tempfile::tempdir().unwrap();
    let mut text =
        "[minecraft]\nworking-directory = '.'\n[launch]\nkind = 'java-jar'\njar = 's.jar'\n#"
            .to_owned();
    text.push_str(&"x".repeat(MAX_CONFIG_BYTES - text.len()));
    assert!(LaunchConfiguration::parse_toml(text.as_bytes(), dir.path()).is_ok());
    text.push('x');
    assert_eq!(
        LaunchConfiguration::parse_toml(text.as_bytes(), dir.path()).unwrap_err(),
        LaunchConfigError::TooLarge
    );
}

#[test]
fn assembled_argument_count_bounds_include_injected_shape_arguments() {
    let dir = tempfile::tempdir().unwrap();
    let args = vec!["'x'"; MAX_ARGS - 2].join(",");
    assert_eq!(
        parse(
            &format!("kind = 'java-jar'\njar = 's.jar'\nargs = [{args}]"),
            dir.path()
        )
        .unwrap()
        .args
        .len(),
        MAX_ARGS
    );
    refused(
        &format!("kind = 'java-jar'\njar = 's.jar'\nargs = [{args},'x']"),
        dir.path(),
    );
    let arg = "x".repeat(MAX_ARG_BYTES);
    assert!(
        parse(
            &format!("kind = 'native'\nexecutable = 'x'\nstop = 'unsupported'\nargs = ['{arg}']"),
            dir.path()
        )
        .is_ok()
    );
    refused(
        &format!("kind = 'native'\nexecutable = 'x'\nstop = 'unsupported'\nargs = ['{arg}x']"),
        dir.path(),
    );
}

#[test]
fn aggregate_argument_and_argfile_bounds_refuse_before_compilation() {
    let dir = tempfile::tempdir().unwrap();
    let args =
        vec![format!("'{}'", "x".repeat(MAX_ARG_BYTES)); MAX_ARGV_BYTES / MAX_ARG_BYTES].join(",");
    refused(
        &format!("kind = 'native'\nexecutable = 'x'\nstop = 'unsupported'\nargs = [{args}]"),
        dir.path(),
    );
    let files = vec!["'args.txt'"; MAX_ARGFILES].join(",");
    assert_eq!(
        parse(
            &format!("kind = 'java-argfile'\nargfiles = [{files}]"),
            dir.path()
        )
        .unwrap()
        .required_files
        .len(),
        MAX_ARGFILES
    );
    refused(
        &format!("kind = 'java-argfile'\nargfiles = [{files}, 'extra.txt']"),
        dir.path(),
    );
}

#[test]
fn debug_and_native_preflight_failure_never_expose_paths_or_arguments() {
    let dir = tempfile::tempdir().unwrap();
    let plan = parse(
        "kind = 'native'\nexecutable = 'TOP-SECRET'\nargs = ['TOP-SECRET']\nstop = 'unsupported'",
        dir.path(),
    )
    .unwrap();
    let debug = format!("{plan:?}");
    assert!(!debug.contains("TOP-SECRET"));
    assert!(!debug.contains(dir.path().to_str().unwrap()));
    let error = plan.into_launch_spec(None).unwrap_err();
    assert_eq!(
        error,
        LaunchConfigError::Preflight(RuntimeError::InvalidLaunch)
    );
    assert_eq!(error.source().unwrap().to_string(), "Game runtime failed");
    assert!(!format!("{error:?}").contains("TOP-SECRET"));
}

#[test]
fn parser_recursion_limit_and_oversized_paths_fail_with_fixed_errors() {
    let dir = tempfile::tempdir().unwrap();
    let nested = format!("{}'TOP-SECRET'{}", "[".repeat(200), "]".repeat(200));
    refused(
        &format!("kind = 'java-jar'\njar = 's.jar'\nargs = {nested}"),
        dir.path(),
    );
    let oversized = "x".repeat(crate::process::MAX_PATH_BYTES + 1);
    refused(
        &format!("kind = 'native'\nexecutable = '{oversized}'\nstop = 'unsupported'"),
        dir.path(),
    );
}

#[cfg(windows)]
#[test]
fn windows_partial_roots_and_drive_relative_paths_are_not_reinterpreted() {
    let dir = tempfile::tempdir().unwrap();
    for path in [
        "C:relative.exe",
        "\\root-relative.exe",
        "/root-relative.exe",
    ] {
        refused(
            &format!("kind = 'native'\nexecutable = '{path}'\nstop = 'unsupported'"),
            dir.path(),
        );
    }
}
