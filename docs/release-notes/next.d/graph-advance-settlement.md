## Behaviour changes

- Run settlement advances pending Fabric graphs in the same project after
  releasing custody. Dependants start automatically; `graph resume` remains
  manual recovery after a crash when no later run settles (#1712).
