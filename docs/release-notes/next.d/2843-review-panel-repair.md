## Fixes
### Pipelines and graphs

- Review repairs re-plan the full panel when their diff touches protected risk paths, including paths already reviewed. Findings from a failed replanned panel feed the remaining repair cycles; principal redesign notifies the owner through the host and waits for a decision when scope changes or a one-way door is proposed (#2843).
