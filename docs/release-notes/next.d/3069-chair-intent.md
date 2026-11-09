## Behaviour changes
### Sessions and compaction

- The session checkpoint now carries an Intent section and compaction asks the summary for one; the compact hook shows it first and names the host transcript (#3069).
