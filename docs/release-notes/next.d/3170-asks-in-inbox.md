## Breaking changes
### Chair, inbox and console

- **Graph node asks are single open requests in the human inbox, and the checkpoint keeps only a link to each.** The question, options and answer are no longer stored in the graph checkpoint, so a paused graph with an open ask saved by an earlier release must be answered or cancelled before upgrading. A chat answer resumes the node, and saving a checkpoint twice files one request. (#3170)
