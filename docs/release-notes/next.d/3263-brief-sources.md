## Breaking changes
### CLI and install

- **`steer --text-stdin` is removed.** Use `steer TARGET --brief-file -`; `run`, `retry` and `steer` accept either `--brief TEXT` or `--brief-file PATH`, with `-` reading stdin. `run` also keeps its positional brief. (#3078, #3263)
