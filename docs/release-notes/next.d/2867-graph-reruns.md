## Fixes
### Pipelines and graphs

- Repeating a task brief starts new work. Reviews and delivery runs reuse results only for equal submissions at the same working directory and HEAD; explicit operation IDs replay earlier submissions (#2867).
- Lost child receipts can be recovered after a graph settles. Child receipts explain that attached children are cancelled when their parent exits (#2867).
