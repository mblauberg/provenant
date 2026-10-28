## Fixes

- Shell lanes can run the system `python3`. `/usr/bin/python3` runs the Command Line Tools' Python, which lives in a framework outside `usr/bin`, so the sandbox now grants exec on that framework (#1950).
