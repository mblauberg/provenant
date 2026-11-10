## Changes
### Providers

- A launch naming a model the selected account does not offer is refused before the provider starts, and the refusal lists the models the account offers. Agy accounts report their models through the usage probe (#3143).
- A failed lane's provider error shows the provider's whole error message from its structured result (bounded), not only the last stderr line (#3143).
