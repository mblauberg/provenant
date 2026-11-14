# Lens: flexibility

Shared guidance: [review-lenses.md](../review-lenses.md).

- Identify hard-coded names and values that should be configuration.
- Check coupling to a single provider, platform or account.
- Can foreseeable extensions fit without costly indirection?
- Flag a lane or launch refusal, or a guard that blocks ordinary work, outside
  the hard boundaries the repository declares as a defect to loosen: prefer
  warn-and-continue or a configurable default.
