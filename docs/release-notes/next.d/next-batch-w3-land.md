## Added

### Landing

- `provenant land` can publish with the owner's admin authority when a ruleset
  demands a review the authenticated owner cannot give, if the host config
  grants `domains.pr.land.admin_publication = "when_review_unavailable"`; the
  retry runs only after land's gate passed and the receipt records the grant
  and its source. Batches derive their target from the members and pin a
  reviewed head per member with `--head N=SHA`. (#3191)
