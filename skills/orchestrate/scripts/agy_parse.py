import json
import re
import sys
from pathlib import Path


def reject_duplicate_members(pairs):
    value = {}
    for key, item in pairs:
        if key in value:
            raise ValueError("duplicate JSON member: %s" % key)
        value[key] = item
    return value


def parse_response(raw_path: Path, diag_path: Path, clean_path: Path, exit_code: int) -> str:
    try:
        stdout = raw_path.read_bytes().decode("utf-8")
        stdout_valid = True
    except UnicodeDecodeError:
        stdout = ""
        stdout_valid = False
    stderr = diag_path.read_text(encoding="utf-8", errors="replace")
    denial = re.compile(r'no output produced.*?a tool required the "[^"]+" permission', re.I | re.S)
    auth = re.compile(r"unauthenticated|not logged in|sign in|quota|rate.?limit|401|403", re.I)
    envelope = None

    if denial.search(stderr):
        status = "permission_denied"
        response = ""
    elif not stdout_valid:
        status = "invalid_envelope"
        response = ""
    else:
        try:
            candidate = json.loads(stdout, object_pairs_hook=reject_duplicate_members)
            if isinstance(candidate, dict):
                envelope = candidate
        except (json.JSONDecodeError, ValueError):
            envelope = None
        if envelope is None:
            if auth.search(stderr) or auth.search(stdout):
                status = "auth_or_quota_error"
            elif exit_code == 0:
                status = "empty_output"
            else:
                status = "error"
            response = ""
        else:
            provider_status = envelope.get("status")
            response = envelope.get("response")
            error_value = envelope.get("error")
            denied_actions = envelope.get("denied_actions")
            error = "" if error_value is None else str(error_value).strip()
            if not isinstance(provider_status, str) or not isinstance(response, str):
                status = "invalid_envelope"
                response = ""
            elif denied_actions is not None and not isinstance(denied_actions, list):
                status = "invalid_envelope"
                response = ""
            elif denied_actions:
                status = "permission_denied"
                response = ""
            elif provider_status.upper() == "SUCCESS" and error:
                status = "auth_or_quota_error" if auth.search(error) else "error"
                response = ""
            elif provider_status.upper() == "SUCCESS" and exit_code != 0:
                status = "error"
                response = ""
            elif provider_status.upper() == "SUCCESS" and response.strip():
                status = "ok"
            elif provider_status.upper() == "SUCCESS":
                status = "empty_output"
                response = ""
            else:
                if "timeout" in error.lower():
                    status = "timeout"
                elif auth.search(error) or auth.search(stderr) or auth.search(stdout):
                    status = "auth_or_quota_error"
                else:
                    status = "error"
                response = ""

    clean_path.write_text(response if status == "ok" else "", encoding="utf-8")

    if status != "ok":
        notes = ["agy dispatch failed: status=%s exit=%d" % (status, exit_code)]
        detail = error if envelope else ""
        if envelope and status == "permission_denied" and envelope.get("denied_actions"):
            notes.append("provider denied_actions: " + json.dumps(envelope["denied_actions"], ensure_ascii=True)[:2000])
        if detail:
            notes.append("provider error: %s" % detail)
        elif (
            envelope
            and isinstance(provider_status, str)
            and provider_status.upper() != "SUCCESS"
        ):
            notes.append(
                "provider error: provider returned a non-success status without an error message"
            )
        elif envelope is None and not stderr.strip() and stdout.strip():
            notes.append("unparsed agy stdout: %s" % stdout.strip()[:2000])
        with diag_path.open("a", encoding="utf-8") as handle:
            handle.write("\n".join(notes) + "\n")

    return status


if __name__ == "__main__":
    print(parse_response(*map(Path, sys.argv[1:4]), int(sys.argv[4])))
