use provenant_os::shell::quote;

/// Shell that enforces the lane's `lanes.pr` policy before the real `gh` runs.
/// `never` refuses every PR mutation and forge API write; `draft` permits only
/// draft creation and maintenance, but refuses promotion and other PR writes;
/// `allowed` adds nothing. Messages name the knob and the layer that set it
/// so a refusal is actionable.
pub(super) fn pr_policy_guard(policy: provenant_config::PrPolicy, source: &str) -> String {
    use provenant_config::PrPolicy;
    if policy == PrPolicy::Allowed {
        return String::new();
    }
    // Fields imply POST only when no method overrides them. Resolve the method
    // after scanning all arguments, including fields that precede the override.
    let mut guard = String::from(
        r#"provenant_forge_write() {
  [ "$1" = api ] || return 1
  shift
  provenant_api_method=
  provenant_api_payload=false
  while [ "$#" -gt 0 ]; do
    case "$1" in
      -f|-F|--field|--raw-field|--input)
        provenant_api_payload=true
        shift; [ "$#" -gt 0 ] || break ;;
      -f?*|-F?*|--field=*|--raw-field=*|--input=*) provenant_api_payload=true ;;
      -X|--method)
        shift; [ "$#" -gt 0 ] || break
        provenant_api_method=$1 ;;
      --method=*) provenant_api_method=${1#--method=} ;;
      -X?*) provenant_api_method=${1#-X} ;;
      -H|--header|--hostname|--cache|-q|--jq|-p|--preview|--template)
        shift; [ "$#" -gt 0 ] || break ;;
    esac
    shift
  done
  case "$provenant_api_method" in
    GET|HEAD|get|head) return 1 ;;
    '') [ "$provenant_api_payload" = true ] && return 0; return 1 ;;
    *) return 0 ;;
  esac
}
"#,
    );
    let (policy_name, message) = match policy {
        PrPolicy::Never => (
            "never",
            format!(
                "error: provenant: pull request and forge writes are refused (lanes.pr=never, source: {source}); the chair publishes after settlement"
            ),
        ),
        PrPolicy::Draft => (
            "draft",
            format!(
                "error: provenant: this lane may open only draft pull requests (lanes.pr=draft, source: {source}); this PR operation or forge write is refused"
            ),
        ),
        PrPolicy::Allowed => unreachable!(),
    };
    let message = quote(&message);
    // Classify the supported gh PR commands in one place. Unknown commands are
    // refused until classified, so a new gh mutation cannot silently bypass policy.
    // A function preserves the caller's arguments and avoids Bash 3.2 parsing
    // case patterns inside command substitution on macOS.
    guard.push_str(&format!(
        "provenant_classify_pr() {{\nprovenant_pr_action=read\nif [ \"$1\" = pr ]; then\n  shift\n  while [ \"$#\" -gt 0 ]; do\n    case \"$1\" in\n      -R|--repo) shift; [ \"$#\" -gt 0 ] || break; shift ;;\n      -R?*|--repo=*) shift ;;\n      *) break ;;\n    esac\n  done\n  case \"$1\" in\n    ''|--help|-h|checkout|checks|diff|list|status|view) ;;\n    create) provenant_pr_action=create ;;\n    edit|close|comment|review) provenant_pr_action=maintenance ;;\n    lock|unlock|update-branch|revert|ready|reopen|merge) provenant_pr_action=write ;;\n    *) provenant_pr_action=write ;;\n  esac\nfi\n}}\nprovenant_classify_pr \"$@\"\ncase \"{policy_name}:$provenant_pr_action\" in\n  never:create|never:maintenance|never:write|draft:write)\n    printf '%s\\n' {message} >&2\n    exit 2 ;;\n  draft:create)\n    provenant_has_draft=false\n    for provenant_arg in \"$@\"; do\n      case $provenant_arg in --draft) provenant_has_draft=true ;; esac\n    done\n    if [ \"$provenant_has_draft\" = false ]; then set -- \"$@\" --draft; fi ;;\nesac\nif provenant_forge_write \"$@\"; then\n  printf '%s\\n' {message} >&2\n  exit 2\nfi\n"
    ));
    guard
}
