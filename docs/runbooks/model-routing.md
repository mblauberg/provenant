# Model routing

Run `provenant route list` to inspect the catalogue and cached availability.
Use `provenant --json route list` for records, or `--adapter NAME` to narrow it.
Resolve a named route with `provenant route resolve --route NAME`, or select
an exact model with `--model ADAPTER/ID` and optional `--effort LEVEL`.
The `utility` pool sits below junior for mechanical work, trying low-cost
non-training Haiku at low effort and Luna before free DeepSeek and GLM.
Select it with
`--route utility`; `--confidential` excludes training or unclassified members
and fallbacks. Free pool members require `--allow-free` (default false), even
when they do not train. It uses exact routing, with no classifier or synonyms.

The bundled Rust catalogue (`tools/front-door/crates/routing/assets/catalogue.toml`)
is authoritative for this pool. No Rust owner reads `config/model-routing.json`;
that legacy file is awaiting removal in the docs lane. Fabric currently accepts
route preferences but its unavailable submission owner does not resolve them;
there is no Fabric `confidential` preference in this slice.

Explicit fallbacks use `--fallback ADAPTER/ID,ADAPTER/ID`.

Dispatch uses the same route flags. Copy the resolved `adapter/model@effort`
from the attempt record; never infer a model from a tier or a provider name.
Quota observations use `provenant quota show` and `provenant quota tick`.

The Python catalogue, account reset commands, cooldown files and routing
wrappers are retired. The Rust routing/providers/quota owners define the
current records and eligibility policy. Run `provenant help route` for flags.
