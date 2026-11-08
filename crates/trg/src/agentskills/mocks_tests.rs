use super::*;
use tempfile::tempdir;

fn write_mock(dir: &Path, server: &str, tool: &str, content: &str) {
    let server_dir = dir.join(server);
    fs::create_dir_all(&server_dir).unwrap();
    fs::write(server_dir.join(format!("{tool}.md")), content).unwrap();
}

#[test]
fn a_suite_with_no_mocks_directory_resolves_to_an_empty_set() {
    let temp = tempdir().unwrap();
    let skill = temp.path().join("skill");
    fs::create_dir_all(skill.join("evals/one")).unwrap();
    let set = resolve_mock_set(&skill, &EvalDirName::default(), "one").unwrap();
    assert!(set.is_empty());
}

#[test]
fn fixed_mock_parses_expect_and_body() {
    let temp = tempdir().unwrap();
    let skill = temp.path().join("skill");
    let suite_mocks = skill.join("evals/mocks");
    write_mock(
        &suite_mocks,
        "github",
        "create_issue",
        "---\ntype: fixed\nexpect:\n  repo: /^acme\\//\n  title: string\n---\n{\"id\": 1}",
    );
    fs::create_dir_all(skill.join("evals/one")).unwrap();

    let set = resolve_mock_set(&skill, &EvalDirName::default(), "one").unwrap();
    let server = ServerName::from("github");
    let tool = ToolName::from("create_issue");
    let declaration = set.tools_for(&server).unwrap().get(&tool).unwrap();
    assert_eq!(declaration.mock_type, MockType::Fixed);
    assert_eq!(declaration.body, "{\"id\": 1}");
    assert_eq!(
        declaration.expect.get(&ExpectPath("repo".to_string())),
        Some(&ExpectConstraint::Regex {
            pattern: RegexPattern::parse("^acme\\/").unwrap()
        })
    );
    assert_eq!(
        declaration.expect.get(&ExpectPath("title".to_string())),
        Some(&ExpectConstraint::TypeName {
            type_name: JsonTypeName::String
        })
    );
}

#[test]
fn agent_mock_type_is_rejected_by_name() {
    let temp = tempdir().unwrap();
    let skill = temp.path().join("skill");
    let suite_mocks = skill.join("evals/mocks");
    write_mock(&suite_mocks, "github", "create_issue", "---\ntype: agent\n---\nbody");
    fs::create_dir_all(skill.join("evals/one")).unwrap();

    let err = resolve_mock_set(&skill, &EvalDirName::default(), "one").unwrap_err();
    let message = err.to_string();
    assert!(
        message.contains("agent"),
        "error should name the rejected type: {message}"
    );
    assert!(message.contains("not yet supported"));
}

#[test]
fn per_case_mock_overrides_suite_mock_file_by_file() {
    let temp = tempdir().unwrap();
    let skill = temp.path().join("skill");
    write_mock(
        &skill.join("evals/mocks"),
        "github",
        "create_issue",
        "---\ntype: fixed\n---\nsuite body",
    );
    write_mock(
        &skill.join("evals/mocks"),
        "github",
        "close_issue",
        "---\ntype: fixed\n---\nsuite close",
    );
    write_mock(
        &skill.join("evals/one/mocks"),
        "github",
        "create_issue",
        "---\ntype: fixed\n---\ncase body",
    );

    let set = resolve_mock_set(&skill, &EvalDirName::default(), "one").unwrap();
    let server = ServerName::from("github");
    let tools = set.tools_for(&server).unwrap();
    assert_eq!(tools.get(&ToolName::from("create_issue")).unwrap().body, "case body");
    assert_eq!(tools.get(&ToolName::from("close_issue")).unwrap().body, "suite close");
}

#[test]
fn file_reference_is_resolved_eagerly_relative_to_the_mock_file_it_is_written_in() {
    let temp = tempdir().unwrap();
    let skill = temp.path().join("skill");
    let suite_mocks = skill.join("evals/mocks");
    write_mock(
        &suite_mocks,
        "github",
        "create_issue",
        "---\ntype: fixed\n---\n{{file:payload.json}}",
    );
    fs::write(suite_mocks.join("github/payload.json"), "{\"ok\":true}").unwrap();
    fs::create_dir_all(skill.join("evals/one")).unwrap();

    let set = resolve_mock_set(&skill, &EvalDirName::default(), "one").unwrap();
    let declaration = set
        .tools_for(&ServerName::from("github"))
        .unwrap()
        .get(&ToolName::from("create_issue"))
        .unwrap();
    assert_eq!(declaration.body, "{\"ok\":true}");
}

/// Resolving from the `mocks/` root instead would make the name `payload.json` mean one
/// file for the whole suite, so the second server to claim it would silently answer with
/// the first server's body.
#[test]
fn two_servers_can_each_ship_a_payload_of_the_same_name() {
    let temp = tempdir().unwrap();
    let skill = temp.path().join("skill");
    let suite_mocks = skill.join("evals/mocks");
    for (server, body) in [("github", "from github"), ("issues", "from issues")] {
        write_mock(
            &suite_mocks,
            server,
            "create_issue",
            "---\ntype: fixed\n---\n{{file:payload.json}}",
        );
        fs::write(suite_mocks.join(server).join("payload.json"), body).unwrap();
    }
    fs::create_dir_all(skill.join("evals/one")).unwrap();

    let set = resolve_mock_set(&skill, &EvalDirName::default(), "one").unwrap();

    for (server, body) in [("github", "from github"), ("issues", "from issues")] {
        let declaration = set
            .tools_for(&ServerName::from(server))
            .unwrap()
            .get(&ToolName::from("create_issue"))
            .unwrap();
        assert_eq!(declaration.body, body, "{server} must answer with its own payload");
    }
}

/// Per-server resolution must not cost an author the ability to share one fixture between
/// servers, which is the thing resolving from the root bought.
#[test]
fn a_file_reference_reaches_a_fixture_shared_above_the_server_directory() {
    let temp = tempdir().unwrap();
    let skill = temp.path().join("skill");
    let suite_mocks = skill.join("evals/mocks");
    write_mock(
        &suite_mocks,
        "github",
        "create_issue",
        "---\ntype: fixed\n---\n{{file:../shared.json}}",
    );
    fs::write(suite_mocks.join("shared.json"), "shared body").unwrap();
    fs::create_dir_all(skill.join("evals/one")).unwrap();

    let set = resolve_mock_set(&skill, &EvalDirName::default(), "one").unwrap();
    let declaration = set
        .tools_for(&ServerName::from("github"))
        .unwrap()
        .get(&ToolName::from("create_issue"))
        .unwrap();
    assert_eq!(declaration.body, "shared body");
}

#[test]
fn missing_file_reference_is_a_hard_error_not_an_empty_string() {
    let temp = tempdir().unwrap();
    let skill = temp.path().join("skill");
    let suite_mocks = skill.join("evals/mocks");
    write_mock(
        &suite_mocks,
        "github",
        "create_issue",
        "---\ntype: fixed\n---\n{{file:missing.json}}",
    );
    fs::create_dir_all(skill.join("evals/one")).unwrap();

    let err = resolve_mock_set(&skill, &EvalDirName::default(), "one").unwrap_err();
    assert!(matches!(err, MocksError::FileReferenceNotFound { .. }));
    assert_eq!(
        err.to_string(),
        format!(
            "{}: `{{{{file:missing.json}}}}` does not resolve to a file",
            suite_mocks.join("github/create_issue.md").display()
        )
    );
    let source = std::error::Error::source(&err)
        .and_then(|source| source.downcast_ref::<std::io::Error>())
        .expect("missing file reference preserves its IO cause");
    assert_eq!(source.kind(), std::io::ErrorKind::NotFound);
}

/// `nonexistent.json` is deliberate: `resolve_file_references` runs only over the parsed
/// body, never over `error:`, so this must parse and answer without ever touching disk
/// for that name. If `resolve_file_references` is ever made to cover `error:` too, this
/// fails loudly with `FileReferenceNotFound` instead of silently passing.
#[test]
fn a_file_placeholder_in_the_error_field_is_never_resolved() {
    let temp = tempdir().unwrap();
    let skill = temp.path().join("skill");
    let suite_mocks = skill.join("evals/mocks");
    write_mock(
        &suite_mocks,
        "github",
        "create_issue",
        "---\ntype: fixed\nerror: \"{{file:nonexistent.json}}\"\n---\n",
    );
    fs::create_dir_all(skill.join("evals/one")).unwrap();

    let set = resolve_mock_set(&skill, &EvalDirName::default(), "one").unwrap();
    let declaration = set
        .tools_for(&ServerName::from("github"))
        .unwrap()
        .get(&ToolName::from("create_issue"))
        .unwrap();
    let server = ServerName::from("github");
    let tool = ToolName::from("create_issue");

    let outcome = resolve_call(declaration, &server, &tool, &serde_json::json!({}));

    assert_eq!(
        outcome.answer,
        MockCallAnswer::Answered {
            text: "{{file:nonexistent.json}}".to_string(),
            is_error: true
        }
    );
}

#[test]
fn content_hash_changes_when_a_mock_body_changes() {
    let temp = tempdir().unwrap();
    let skill = temp.path().join("skill");
    write_mock(
        &skill.join("evals/mocks"),
        "github",
        "create_issue",
        "---\ntype: fixed\n---\nfirst",
    );
    fs::create_dir_all(skill.join("evals/one")).unwrap();
    let first = resolve_mock_set(&skill, &EvalDirName::default(), "one")
        .unwrap()
        .content_hash();

    write_mock(
        &skill.join("evals/mocks"),
        "github",
        "create_issue",
        "---\ntype: fixed\n---\nsecond",
    );
    let second = resolve_mock_set(&skill, &EvalDirName::default(), "one")
        .unwrap()
        .content_hash();

    assert_ne!(first, second);
}

#[test]
fn content_hash_changes_when_a_referenced_file_changes() {
    let temp = tempdir().unwrap();
    let skill = temp.path().join("skill");
    let suite_mocks = skill.join("evals/mocks");
    write_mock(
        &suite_mocks,
        "github",
        "create_issue",
        "---\ntype: fixed\n---\n{{file:payload.json}}",
    );
    let payload = suite_mocks.join("github/payload.json");
    fs::write(&payload, "one").unwrap();
    fs::create_dir_all(skill.join("evals/one")).unwrap();

    let first = resolve_mock_set(&skill, &EvalDirName::default(), "one")
        .unwrap()
        .content_hash();
    fs::write(&payload, "two").unwrap();
    let second = resolve_mock_set(&skill, &EvalDirName::default(), "one")
        .unwrap()
        .content_hash();

    assert_ne!(
        first, second,
        "a file a mock references by {{file:...}} is a run input just as much as the mock text itself"
    );
}

#[test]
fn a_mock_set_with_an_expect_constraint_round_trips_through_content_hash() {
    let mut servers = BTreeMap::new();
    let mut tools = BTreeMap::new();
    tools.insert(
        ToolName::from("create_issue"),
        MockDeclaration {
            mock_type: MockType::Fixed,
            expect: BTreeMap::from([(
                ExpectPath("repo".to_string()),
                ExpectConstraint::Regex {
                    pattern: RegexPattern::parse("^acme/").unwrap(),
                },
            )]),
            error: None,
            body: "{}".to_string(),
        },
    );
    servers.insert(ServerName::from("github"), tools);
    let set = MockSet { servers };

    set.content_hash();
}

#[test]
fn expect_violation_reports_path_constraint_and_received_value() {
    let declaration = MockDeclaration {
        mock_type: MockType::Fixed,
        expect: BTreeMap::from([(
            ExpectPath("repo".to_string()),
            ExpectConstraint::Regex {
                pattern: RegexPattern::parse("^acme/").unwrap(),
            },
        )]),
        error: None,
        body: "{}".to_string(),
    };
    let server = ServerName::from("github");
    let tool = ToolName::from("create_issue");
    let input = serde_json::json!({"repo": "other/repo"});

    let violations = declaration.check_expectations(&server, &tool, &input);
    assert_eq!(violations.len(), 1);
    assert_eq!(violations[0].path.as_str(), "repo");
    assert_eq!(violations[0].received, serde_json::json!("other/repo"));
}

#[test]
fn expect_satisfied_produces_no_violation() {
    let declaration = MockDeclaration {
        mock_type: MockType::Fixed,
        expect: BTreeMap::from([(
            ExpectPath("repo".to_string()),
            ExpectConstraint::Regex {
                pattern: RegexPattern::parse("^acme/").unwrap(),
            },
        )]),
        error: None,
        body: "{}".to_string(),
    };
    let server = ServerName::from("github");
    let tool = ToolName::from("create_issue");
    let input = serde_json::json!({"repo": "acme/widgets"});

    assert!(declaration.check_expectations(&server, &tool, &input).is_empty());
}

#[test]
fn a_mock_type_reads_back_from_json_as_the_word_its_frontmatter_spells() {
    assert_eq!(
        serde_json::to_value(MockType::Fixed).unwrap(),
        serde_json::json!("fixed")
    );
    assert_eq!(
        serde_json::from_value::<MockType>(serde_json::json!("fixed")).unwrap(),
        MockType::Fixed,
        "the frontmatter parser and the resolved-json reader answer to one vocabulary"
    );
}

#[test]
fn every_json_type_name_reads_back_from_json_as_the_word_expect_spells() {
    for name in [
        JsonTypeName::String,
        JsonTypeName::Number,
        JsonTypeName::Boolean,
        JsonTypeName::Object,
        JsonTypeName::Array,
    ] {
        assert_eq!(
            serde_json::to_value(name).unwrap(),
            serde_json::json!(name.label()),
            "{name:?}"
        );
        assert_eq!(JsonTypeName::parse(name.label()), Some(name), "{name:?}");
    }
}

#[test]
fn input_placeholder_substitutes_from_call_arguments() {
    let server = ServerName::from("github");
    let tool = ToolName::from("create_issue");
    let input = serde_json::json!({"repo": "acme/widgets"});
    let out = substitute_input_placeholders("repo is {{input.repo}}", &server, &tool, &input).unwrap();
    assert_eq!(out, "repo is acme/widgets");
}

#[test]
fn unresolved_input_placeholder_is_a_call_time_error_not_an_empty_string() {
    let server = ServerName::from("github");
    let tool = ToolName::from("create_issue");
    let input = serde_json::json!({});
    let err = substitute_input_placeholders("repo is {{input.repo}}", &server, &tool, &input).unwrap_err();
    assert_eq!(err.dotted_path, "repo");
}

#[test]
fn resolve_call_answers_body_and_still_reports_a_violation() {
    let declaration = MockDeclaration {
        mock_type: MockType::Fixed,
        expect: BTreeMap::from([(
            ExpectPath("repo".to_string()),
            ExpectConstraint::Regex {
                pattern: RegexPattern::parse("^acme/").unwrap(),
            },
        )]),
        error: None,
        body: "{{input.repo}} created".to_string(),
    };
    let server = ServerName::from("github");
    let tool = ToolName::from("create_issue");
    let input = serde_json::json!({"repo": "other/repo"});

    let outcome = resolve_call(&declaration, &server, &tool, &input);
    assert_eq!(
        outcome.answer,
        MockCallAnswer::Answered {
            text: "other/repo created".to_string(),
            is_error: false
        }
    );
    assert_eq!(
        outcome.violations.len(),
        1,
        "the call still answers per its declaration even though the input violated expect"
    );
}

#[test]
fn resolve_call_with_a_declared_error_answers_as_a_tool_error() {
    let declaration = MockDeclaration {
        mock_type: MockType::Fixed,
        expect: BTreeMap::new(),
        error: Some("rate limited".to_string()),
        body: "unused".to_string(),
    };
    let server = ServerName::from("github");
    let tool = ToolName::from("create_issue");
    let input = serde_json::json!({});

    let outcome = resolve_call(&declaration, &server, &tool, &input);
    assert_eq!(
        outcome.answer,
        MockCallAnswer::Answered {
            text: "rate limited".to_string(),
            is_error: true
        }
    );
}

#[test]
fn resolve_call_fails_the_call_on_an_unresolved_placeholder() {
    let declaration = MockDeclaration {
        mock_type: MockType::Fixed,
        expect: BTreeMap::new(),
        error: None,
        body: "{{input.missing}}".to_string(),
    };
    let server = ServerName::from("github");
    let tool = ToolName::from("create_issue");
    let input = serde_json::json!({});

    let outcome = resolve_call(&declaration, &server, &tool, &input);
    assert!(matches!(outcome.answer, MockCallAnswer::Unresolved(_)));
}

#[test]
fn resolve_call_still_reports_a_violation_when_substitution_fails() {
    let declaration = MockDeclaration {
        mock_type: MockType::Fixed,
        expect: BTreeMap::from([(
            ExpectPath("repo".to_string()),
            ExpectConstraint::Regex {
                pattern: RegexPattern::parse("^acme/").unwrap(),
            },
        )]),
        error: None,
        body: "{{input.missing}}".to_string(),
    };
    let server = ServerName::from("github");
    let tool = ToolName::from("create_issue");
    let input = serde_json::json!({"repo": "other/repo"});

    let outcome = resolve_call(&declaration, &server, &tool, &input);

    assert!(matches!(outcome.answer, MockCallAnswer::Unresolved(_)));
    assert_eq!(
        outcome.violations.len(),
        1,
        "a call that breaks expect and then fails substitution still earned the violation"
    );
}

/// Every path in the materialized config is resolved by somebody else. The harness is
/// started with its working directory set to the run's workspace and it spawns the mock
/// server from there, so a relative `--out-dir` handed through unchanged would send all
/// of them looking under the workspace. With `--strict-mcp-config` the harness then
/// refuses to start and an ordinary invocation cannot run a mocked case at all.
#[test]
fn a_relative_run_directory_still_names_the_mocks_somewhere_a_child_process_can_find_them() {
    let temp = tempdir().unwrap();
    let skill = temp.path().join("skill");
    write_mock(
        &skill.join("evals/mocks"),
        "github",
        "create_issue",
        "---\ntype: fixed\n---\ncreated",
    );
    let mock_set = resolve_mock_set(&skill, &EvalDirName::default(), "one").unwrap();
    let run_dir = relative_to_current_dir(&temp.path().join("run-dir"));
    assert!(
        run_dir.is_relative(),
        "the point of this test is a run directory that means different things from different places"
    );

    let config = materialize_mock_set(
        &mock_set,
        &run_dir,
        &MockServerBinary::at("/usr/local/bin/trg").unwrap(),
        &MockServerEnv::none(),
    )
    .unwrap();

    for path in [config.json(), config.toml()] {
        assert!(
            path.is_absolute(),
            "the harness is handed this path and reads it from the workspace, not from here: {path:?}"
        );
    }
    let config: serde_json::Value = serde_json::from_str(&fs::read_to_string(config.json()).unwrap()).unwrap();
    let args: Vec<String> = config["mcpServers"]["github"]["args"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap().to_string())
        .collect();
    for flag in ["--mocks-dir", "--calls-file"] {
        let position = args.iter().position(|arg| arg == flag).expect("the flag is written");
        let value = Path::new(&args[position + 1]);
        assert!(
            value.is_absolute(),
            "{flag} is resolved by the mock server, not by us: {value:?}"
        );
        assert!(
            value.starts_with(temp.path().canonicalize().unwrap()),
            "{flag} points into the run: {value:?}"
        );
    }
}

/// A path to `target` that only means `target` from the current working directory, built
/// without moving the process, which every other test in this binary shares.
fn relative_to_current_dir(target: &Path) -> PathBuf {
    let here = std::env::current_dir().unwrap().canonicalize().unwrap();
    let mut path = PathBuf::new();
    for _ in here.components().skip(1) {
        path.push("..");
    }
    let anchored = target
        .parent()
        .expect("the run directory has a parent")
        .canonicalize()
        .unwrap()
        .join(target.file_name().expect("the run directory is named"));
    for component in anchored.components().skip(1) {
        path.push(component);
    }
    path
}

/// A mock whose `expect` names a regex that cannot compile is an authoring mistake and
/// has to be reported as one. Compiling it at match time instead turns it into an
/// expectation nothing can satisfy, which blames the run for the author's typo.
#[test]
fn an_expect_regex_that_cannot_compile_fails_the_load() {
    let temp = tempdir().unwrap();
    let skill = temp.path().join("skill");
    write_mock(
        &skill.join("evals/mocks"),
        "github",
        "create_issue",
        "---\ntype: fixed\nexpect:\n  repo: \"/acme(/\"\n---\n{}",
    );

    let error = resolve_mock_set(&skill, &EvalDirName::default(), "one")
        .expect_err("an uncompilable expect regex must not load");

    assert!(
        matches!(
            &error,
            MocksError::InvalidExpectRegex { expect_path, pattern, .. }
                if expect_path == "repo" && pattern == "acme("
        ),
        "unexpected error: {error}"
    );
}

/// The harness spawns the mock server with its working directory set to the run's
/// workspace rather than to wherever `trg` was invoked from, so a relative command is
/// looked up in the wrong place and the server simply never starts.
#[test]
fn a_relative_binary_cannot_be_handed_to_a_mock_server() {
    let error = MockServerBinary::at("target/debug/trg").expect_err("a relative binary must be refused");

    assert!(
        matches!(&error, MocksError::RelativeMockServerBinary { path } if path == Path::new("target/debug/trg")),
        "unexpected error: {error}"
    );
}

#[test]
fn materialize_writes_one_json_file_per_tool_and_an_mcp_config_naming_the_mock_server_subcommand() {
    let temp = tempdir().unwrap();
    let skill = temp.path().join("skill");
    write_mock(
        &skill.join("evals/mocks"),
        "github",
        "create_issue",
        "---\ntype: fixed\n---\ncreated",
    );
    let mock_set = resolve_mock_set(&skill, &EvalDirName::default(), "one").unwrap();
    let run_dir = temp.path().join("run-dir");
    let trg_binary = MockServerBinary::at("/usr/local/bin/trg").unwrap();

    let config = materialize_mock_set(&mock_set, &run_dir, &trg_binary, &MockServerEnv::none()).unwrap();
    let run_dir = run_dir.canonicalize().unwrap();

    let tool_json_path = run_dir.join("mcp-mocks/github/create_issue.json");
    assert!(tool_json_path.is_file());
    let declaration: MockDeclaration = serde_json::from_str(&fs::read_to_string(&tool_json_path).unwrap()).unwrap();
    assert_eq!(declaration.body, "created");

    let document: serde_json::Value = serde_json::from_str(&fs::read_to_string(config.json()).unwrap()).unwrap();
    let github_server = &document["mcpServers"]["github"];
    assert_eq!(github_server["command"], "/usr/local/bin/trg");
    let args: Vec<String> = github_server["args"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap().to_string())
        .collect();
    assert_eq!(
        args,
        vec![
            "ai",
            "skills",
            "eval",
            "mock-server",
            "--mocks-dir",
            run_dir.join("mcp-mocks/github").to_string_lossy().as_ref(),
            "--server",
            "github",
            "--calls-file",
            run_dir.join(MOCK_CALLS_LOG_NAME).to_string_lossy().as_ref(),
        ]
    );
}

/// The two files are the same server table in two formats, and the harness that reads
/// one never sees the other. Rendering them from separate builders is how the JSON a
/// report keeps as the record of what was injected and the TOML a codex run actually
/// injected would come to disagree without anything failing.
#[test]
fn the_toml_a_codex_run_is_given_names_the_same_servers_as_the_json_kept_beside_it() {
    let temp = tempdir().unwrap();
    let skill = temp.path().join("skill");
    write_mock(
        &skill.join("evals/mocks"),
        "github",
        "create_issue",
        "---\ntype: fixed\n---\ncreated",
    );
    write_mock(
        &skill.join("evals/mocks"),
        "linear",
        "search",
        "---\ntype: fixed\n---\nfound",
    );
    let mock_set = resolve_mock_set(&skill, &EvalDirName::default(), "one").unwrap();
    let run_dir = temp.path().join("run-dir");

    let config = materialize_mock_set(
        &mock_set,
        &run_dir,
        &MockServerBinary::at("/usr/local/bin/trg").unwrap(),
        &MockServerEnv::none(),
    )
    .unwrap();

    let from_json: serde_json::Value = serde_json::from_str(&fs::read_to_string(config.json()).unwrap()).unwrap();
    let from_toml: toml::Value = toml::from_str(&fs::read_to_string(config.toml()).unwrap())
        .expect("the file handed to codex as its config.toml must parse as toml");

    let json_servers = from_json["mcpServers"].as_object().expect("the json names servers");
    let toml_servers = from_toml["mcp_servers"].as_table().expect("the toml names servers");
    assert_eq!(json_servers.len(), toml_servers.len());
    for (server, declared) in json_servers {
        let mirrored = toml_servers
            .get(server)
            .unwrap_or_else(|| panic!("{server} is in both"));
        assert_eq!(
            declared["command"].as_str().unwrap(),
            mirrored["command"].as_str().unwrap()
        );
        let json_args: Vec<&str> = declared["args"]
            .as_array()
            .unwrap()
            .iter()
            .map(|arg| arg.as_str().unwrap())
            .collect();
        let toml_args: Vec<&str> = mirrored["args"]
            .as_array()
            .unwrap()
            .iter()
            .map(|arg| arg.as_str().unwrap())
            .collect();
        assert_eq!(json_args, toml_args);
    }
}

/// A run directory can sit under a path holding a quote, a backslash or a newline, and
/// every path in this table is one a child process has to open. A table rendered by
/// pasting strings together would hand codex a `config.toml` that either fails to parse
/// or names a different file than the one trg wrote.
#[test]
fn a_run_directory_whose_name_needs_escaping_still_names_the_same_paths_through_the_toml() {
    let temp = tempdir().unwrap();
    let skill = temp.path().join("skill");
    write_mock(
        &skill.join("evals/mocks"),
        "github",
        "create_issue",
        "---\ntype: fixed\n---\ncreated",
    );
    let mock_set = resolve_mock_set(&skill, &EvalDirName::default(), "one").unwrap();
    let run_dir = temp.path().join(r#"run "dir" \ with	escapes"#);

    let config = materialize_mock_set(
        &mock_set,
        &run_dir,
        &MockServerBinary::at("/usr/local/bin/trg").unwrap(),
        &MockServerEnv::none(),
    )
    .unwrap();

    let from_toml: toml::Value = toml::from_str(&fs::read_to_string(config.toml()).unwrap())
        .expect("a run directory that needs escaping still produces parseable toml");
    let args: Vec<&str> = from_toml["mcp_servers"]["github"]["args"]
        .as_array()
        .unwrap()
        .iter()
        .map(|arg| arg.as_str().unwrap())
        .collect();
    let mocks_position = args.iter().position(|arg| *arg == "--mocks-dir").unwrap();
    assert_eq!(
        Path::new(args[mocks_position + 1]),
        run_dir.canonicalize().unwrap().join("mcp-mocks/github")
    );
}

/// The call log is a mock server's claim to have come up, so nothing that runs before
/// one does may write it: a file left here would be read as an empty log by a run whose
/// server failed to load its mocks and never answered anything.
#[test]
fn materializing_a_mock_set_leaves_the_call_log_to_the_server_that_comes_up() {
    let temp = tempdir().unwrap();
    let skill = temp.path().join("skill");
    write_mock(
        &skill.join("evals/mocks"),
        "github",
        "create_issue",
        "---\ntype: fixed\n---\ncreated",
    );
    let mock_set = resolve_mock_set(&skill, &EvalDirName::default(), "one").unwrap();
    let run_dir = temp.path().join("run-dir");

    materialize_mock_set(
        &mock_set,
        &run_dir,
        &MockServerBinary::at("/usr/local/bin/trg").unwrap(),
        &MockServerEnv::none(),
    )
    .unwrap();

    assert!(!run_dir.join(MOCK_CALLS_LOG_NAME).exists());
}

#[test]
fn a_materialized_expect_constraint_round_trips_through_the_json_file_the_mock_server_reads() {
    let temp = tempdir().unwrap();
    let skill = temp.path().join("skill");
    write_mock(
        &skill.join("evals/mocks"),
        "github",
        "create_issue",
        "---\ntype: fixed\nexpect:\n  repo: /^acme\\//\n  title: string\n  priority: [low, high]\n---\n{\"id\": 1}",
    );
    let mock_set = resolve_mock_set(&skill, &EvalDirName::default(), "one").unwrap();
    let run_dir = temp.path().join("run-dir");

    materialize_mock_set(
        &mock_set,
        &run_dir,
        &MockServerBinary::at("/usr/local/bin/trg").unwrap(),
        &MockServerEnv::none(),
    )
    .unwrap();

    let tool_json_path = run_dir.join("mcp-mocks/github/create_issue.json");
    let declaration: MockDeclaration = serde_json::from_str(&fs::read_to_string(&tool_json_path).unwrap())
        .expect("a materialized expect constraint must deserialize back into a MockDeclaration");
    assert_eq!(
        declaration.expect.get(&ExpectPath("repo".to_string())),
        Some(&ExpectConstraint::Regex {
            pattern: RegexPattern::parse("^acme\\/").unwrap()
        })
    );
    assert_eq!(
        declaration.expect.get(&ExpectPath("title".to_string())),
        Some(&ExpectConstraint::TypeName {
            type_name: JsonTypeName::String
        })
    );
    assert_eq!(
        declaration.expect.get(&ExpectPath("priority".to_string())),
        Some(&ExpectConstraint::OneOf {
            values: vec!["low".to_string(), "high".to_string()]
        })
    );
}

#[test]
fn a_call_renders_its_input_with_keys_in_a_fixed_order() {
    let written = RenderedMockCalls::render(
        r#"{"server":"github","tool":"create_issue","input":{"title":"t","labels":["bug"],"repo":"acme/w"}}"#,
    );
    let reordered = RenderedMockCalls::render(
        r#"{"server":"github","tool":"create_issue","input":{"repo":"acme/w","title":"t","labels":["bug"]}}"#,
    );

    assert_eq!(
        written.as_str(),
        r#"github.create_issue {"labels":["bug"],"repo":"acme/w","title":"t"}"#
    );
    assert_eq!(
        written, reordered,
        "a pattern written against the rendering must not depend on how a harness ordered the call"
    );
}

#[test]
fn blank_lines_in_a_call_log_are_not_rendered_as_calls() {
    let rendered =
        RenderedMockCalls::render("\n{\"server\":\"github\",\"tool\":\"close_issue\",\"input\":{\"number\":41}}\n\n");

    assert_eq!(rendered.as_str(), r#"github.close_issue {"number":41}"#);
}

#[test]
fn a_call_log_that_was_never_written_reads_as_no_log_rather_than_as_no_calls() {
    let temp = tempdir().unwrap();
    let path = temp.path().join(MOCK_CALLS_LOG_NAME);

    assert_eq!(RenderedMockCalls::read(&path).unwrap(), None);

    fs::write(&path, "").unwrap();
    let rendered = RenderedMockCalls::read(&path)
        .expect("a log that exists is readable")
        .expect("a log that exists is a log");
    assert!(rendered.as_str().is_empty());
}

/// A log the reader broke on is a run that did host a mock server, so answering `None`
/// would hand a caller the one fact it must not conclude from it.
#[test]
fn a_call_log_that_cannot_be_read_is_not_reported_as_a_log_that_is_not_there() {
    let temp = tempdir().unwrap();
    let path = temp.path().join(MOCK_CALLS_LOG_NAME);
    fs::write(&path, [0xff, 0xfe, 0x00]).unwrap();

    let error = RenderedMockCalls::read(&path).expect_err("invalid utf-8 is not an absent log");

    assert_ne!(error.kind(), io::ErrorKind::NotFound);
}

fn exporter_env() -> Vec<(String, String)> {
    [
        ("OTEL_EXPORTER_OTLP_ENDPOINT", "http://collector"),
        ("OTEL_EXPORTER_OTLP_HEADERS", "api-key=secret"),
        ("OTEL_EXPORTER_OTLP_TRACES_HEADERS", "api-key=secret"),
        ("OTEL_EXPORTER_OTLP_CLIENT_KEY", "-----BEGIN PRIVATE KEY-----"),
        ("HOME", "/home/someone"),
    ]
    .map(|(key, value)| (key.to_string(), value.to_string()))
    .to_vec()
}

fn traceparent() -> Vec<(String, String)> {
    vec![("TRACEPARENT".to_string(), "00-abc-def-01".to_string())]
}

#[test]
fn a_pass_with_no_active_trace_gives_mock_servers_no_environment() {
    let env = MockServerEnv::from_parts(Vec::new(), exporter_env(), TelemetryForwarding::On);
    assert!(env.is_empty());
}

#[test]
fn a_pass_that_does_not_forward_telemetry_hands_mock_servers_only_its_trace() {
    let env = MockServerEnv::from_parts(traceparent(), exporter_env(), TelemetryForwarding::Off);

    let handed = env.handed();
    let keys: Vec<&String> = handed.keys().collect();
    assert_eq!(keys, ["TRACEPARENT"]);
}

#[test]
fn a_forwarding_pass_hands_mock_servers_its_exporter_and_headers_but_records_no_headers() {
    let env = MockServerEnv::from_parts(traceparent(), exporter_env(), TelemetryForwarding::On);

    let recorded: Vec<&String> = env.recorded.keys().collect();
    assert_eq!(recorded, ["OTEL_EXPORTER_OTLP_ENDPOINT", "TRACEPARENT"]);
    let handed = env.handed();
    let handed_keys: Vec<&String> = handed.keys().collect();
    assert_eq!(
        handed_keys,
        [
            "OTEL_EXPORTER_OTLP_CLIENT_KEY",
            "OTEL_EXPORTER_OTLP_ENDPOINT",
            "OTEL_EXPORTER_OTLP_HEADERS",
            "OTEL_EXPORTER_OTLP_TRACES_HEADERS",
            "TRACEPARENT"
        ]
    );
}

#[test]
fn a_forwarding_pass_writes_headers_only_into_the_owner_only_config_a_harness_is_handed() {
    let temp = tempdir().unwrap();
    let skill = temp.path().join("skill");
    write_mock(
        &skill.join("evals/mocks"),
        "github",
        "create_issue",
        "---\ntype: fixed\n---\nok\n",
    );
    let mock_set = resolve_mock_set(&skill, &EvalDirName::default(), "case").unwrap();
    let env = MockServerEnv::from_parts(traceparent(), exporter_env(), TelemetryForwarding::On);
    let run_dir = temp.path().join("run");

    let config = materialize_mock_set(
        &mock_set,
        &run_dir,
        &MockServerBinary::at("/usr/local/bin/trg").unwrap(),
        &env,
    )
    .unwrap();

    for record in [MCP_CONFIG_FILE_NAME, MCP_CONFIG_TOML_FILE_NAME] {
        let recorded = fs::read_to_string(run_dir.join(record)).unwrap();
        assert!(recorded.contains("OTEL_EXPORTER_OTLP_ENDPOINT"), "{record}");
        assert!(!recorded.contains("api-key=secret"), "{record}");
        assert!(!recorded.contains("BEGIN PRIVATE KEY"), "{record}");
    }
    for handed in [config.json(), config.toml()] {
        assert_ne!(handed.parent(), Some(run_dir.as_path()));
        let handed_text = fs::read_to_string(handed).unwrap();
        assert!(handed_text.contains("api-key=secret"), "{handed:?}");
        assert!(handed_text.contains("BEGIN PRIVATE KEY"), "{handed:?}");
        let mode = fs::metadata(handed).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "{handed:?}");
    }
}

/// The property the fix exists for: once a harness that was handed the `mcp-launch`
/// copy has exited, nothing under the run directory a report keeps carries the literal
/// header value it was given.
#[test]
fn no_file_in_the_run_directory_carries_the_otel_header_value_after_scrubbing() {
    let temp = tempdir().unwrap();
    let skill = temp.path().join("skill");
    write_mock(
        &skill.join("evals/mocks"),
        "github",
        "create_issue",
        "---\ntype: fixed\n---\nok\n",
    );
    let mock_set = resolve_mock_set(&skill, &EvalDirName::default(), "case").unwrap();
    let env = MockServerEnv::from_parts(traceparent(), exporter_env(), TelemetryForwarding::On);
    let run_dir = temp.path().join("run");

    let config = materialize_mock_set(
        &mock_set,
        &run_dir,
        &MockServerBinary::at("/usr/local/bin/trg").unwrap(),
        &env,
    )
    .unwrap();

    config.scrub().unwrap();

    for entry in walkdir(&run_dir) {
        let contents = fs::read(&entry).unwrap();
        let text = String::from_utf8_lossy(&contents);
        assert!(
            !text.contains("api-key=secret"),
            "`{}` still carries the forwarded otel header value",
            entry.display()
        );
        assert!(
            !text.contains("BEGIN PRIVATE KEY"),
            "`{}` still carries the forwarded otel client key",
            entry.display()
        );
    }
}

#[test]
fn scrubbing_removes_the_whole_launch_directory_so_nothing_handed_to_a_harness_survives() {
    let temp = tempdir().unwrap();
    let skill = temp.path().join("skill");
    write_mock(
        &skill.join("evals/mocks"),
        "github",
        "create_issue",
        "---\ntype: fixed\n---\nok\n",
    );
    let mock_set = resolve_mock_set(&skill, &EvalDirName::default(), "case").unwrap();
    let env = MockServerEnv::from_parts(traceparent(), exporter_env(), TelemetryForwarding::On);
    let run_dir = temp.path().join("run");

    let config = materialize_mock_set(
        &mock_set,
        &run_dir,
        &MockServerBinary::at("/usr/local/bin/trg").unwrap(),
        &env,
    )
    .unwrap();
    let launch_dir = config.json().parent().unwrap().to_path_buf();
    assert!(launch_dir.exists());

    config.scrub().unwrap();

    assert!(
        !launch_dir.exists(),
        "the mcp-launch directory must not survive the harness"
    );
    for record in [MCP_CONFIG_FILE_NAME, MCP_CONFIG_TOML_FILE_NAME] {
        assert!(
            run_dir.join(record).exists(),
            "the record a report keeps beside the run is untouched by scrubbing"
        );
    }
}

#[test]
fn scrubbing_twice_is_a_no_op() {
    let temp = tempdir().unwrap();
    let skill = temp.path().join("skill");
    write_mock(
        &skill.join("evals/mocks"),
        "github",
        "create_issue",
        "---\ntype: fixed\n---\nok\n",
    );
    let mock_set = resolve_mock_set(&skill, &EvalDirName::default(), "case").unwrap();
    let run_dir = temp.path().join("run");

    let config = materialize_mock_set(
        &mock_set,
        &run_dir,
        &MockServerBinary::at("/usr/local/bin/trg").unwrap(),
        &MockServerEnv::none(),
    )
    .unwrap();

    config.scrub().unwrap();
    config.scrub().unwrap();
}

fn walkdir(dir: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    let Ok(entries) = fs::read_dir(dir) else {
        return files;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            files.extend(walkdir(&path));
        } else {
            files.push(path);
        }
    }
    files
}

#[test]
fn a_traced_pass_writes_the_trace_into_every_server_env_block_in_both_formats() {
    let temp = tempdir().unwrap();
    let skill = temp.path().join("skill");
    write_mock(
        &skill.join("evals/mocks"),
        "github",
        "create_issue",
        "---\ntype: fixed\n---\nok\n",
    );
    let mock_set = resolve_mock_set(&skill, &EvalDirName::default(), "case").unwrap();
    let env = MockServerEnv::from_parts(traceparent(), [], TelemetryForwarding::Off);

    let config = materialize_mock_set(
        &mock_set,
        &temp.path().join("run"),
        &MockServerBinary::at("/usr/local/bin/trg").unwrap(),
        &env,
    )
    .unwrap();

    let json: serde_json::Value = serde_json::from_str(&fs::read_to_string(config.json()).unwrap()).unwrap();
    assert_eq!(json["mcpServers"]["github"]["env"]["TRACEPARENT"], "00-abc-def-01");
    let toml: toml::Value = toml::from_str(&fs::read_to_string(config.toml()).unwrap()).unwrap();
    assert_eq!(
        toml["mcp_servers"]["github"]["env"]["TRACEPARENT"].as_str(),
        Some("00-abc-def-01")
    );
}

#[test]
fn an_untraced_pass_writes_no_env_block() {
    let temp = tempdir().unwrap();
    let skill = temp.path().join("skill");
    write_mock(
        &skill.join("evals/mocks"),
        "github",
        "create_issue",
        "---\ntype: fixed\n---\nok\n",
    );
    let mock_set = resolve_mock_set(&skill, &EvalDirName::default(), "case").unwrap();

    let config = materialize_mock_set(
        &mock_set,
        &temp.path().join("run"),
        &MockServerBinary::at("/usr/local/bin/trg").unwrap(),
        &MockServerEnv::none(),
    )
    .unwrap();

    assert!(!fs::read_to_string(config.json()).unwrap().contains("\"env\""));
    assert!(!fs::read_to_string(config.toml()).unwrap().contains("env"));
}

#[test]
fn a_call_log_line_written_before_timestamps_still_reads_back() {
    let entry: MockCallLogEntry =
        serde_json::from_str(r#"{"server":"github","tool":"create_issue","input":{}}"#).unwrap();
    assert_eq!(entry.received_at, None);
}

#[test]
fn a_call_timestamp_round_trips_through_the_log() {
    let entry = MockCallLogEntry {
        server: ServerName::from("github"),
        tool: ToolName::from("create_issue"),
        input: serde_json::json!({}),
        violations: Vec::new(),
        received_at: Some(MockCallTimestamp::now()),
    };
    let line = serde_json::to_string(&entry).unwrap();
    assert_eq!(serde_json::from_str::<MockCallLogEntry>(&line).unwrap(), entry);
}
