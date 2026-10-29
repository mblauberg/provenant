"""Keep fixture repositories separate from a writer lane's Git environment."""
import os


def isolate():
    for name in list(os.environ):
        if name.startswith("GIT_"):
            del os.environ[name]
    os.environ["PATH"] = "/opt/homebrew/bin:/usr/local/bin:/opt/local/bin:/usr/bin:/bin:" + os.environ.get("PATH", "")
    os.environ["GIT_CONFIG_GLOBAL"] = "/dev/null"
    os.environ["GIT_CONFIG_NOSYSTEM"] = "1"
