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
    // Normalise selectors at each command boundary in one place, without
    // changing the arguments forwarded to gh. API options are scanned only
    // after its command boundary, so option values cannot become commands.
    let mut guard = String::from(
        r#"provenant_classify() {
  provenant_pr_action=read
  provenant_forge_write=false
  provenant_command=
  while [ "$#" -gt 0 ]; do
    case "$1" in
      -R|--repo|--hostname)
        shift; [ "$#" -gt 0 ] || return 0; shift ;;
      -R?*|--repo=*|--hostname=*) shift ;;
      pr)
        if [ -n "$provenant_command" ]; then break; fi
        provenant_command=pr; shift ;;
      *) break ;;
    esac
  done
  if [ "$provenant_command" = pr ]; then
    case "$1" in
      ''|--help|-h|checkout|checks|diff|list|status|view) ;;
      create) provenant_pr_action=create ;;
      edit|close|comment|review) provenant_pr_action=maintenance ;;
      *) provenant_pr_action=write ;;
    esac
    return
  fi
  [ "$1" = api ] || return 0
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
      -H|--header|--hostname|-R|--repo|--cache|-q|--jq|-p|--preview|-t|--template)
        shift; [ "$#" -gt 0 ] || break ;;
    esac
    shift
  done
  case "$provenant_api_method" in
    GET|HEAD|get|head) ;;
    '') [ "$provenant_api_payload" = false ] || provenant_forge_write=true ;;
    *) provenant_forge_write=true ;;
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
    guard.push_str(&format!(
        "provenant_classify \"$@\"\ncase \"{policy_name}:$provenant_pr_action\" in\n  never:create|never:maintenance|never:write|draft:write)\n    printf '%s\\n' {message} >&2\n    exit 2 ;;\n  draft:create)\n    provenant_has_draft=false\n    for provenant_arg in \"$@\"; do\n      case $provenant_arg in --draft) provenant_has_draft=true ;; esac\n    done\n    if [ \"$provenant_has_draft\" = false ]; then set -- \"$@\" --draft; fi ;;\nesac\nif [ \"$provenant_forge_write\" = true ]; then\n  printf '%s\\n' {message} >&2\n  exit 2\nfi\n"
    ));
    guard
}
