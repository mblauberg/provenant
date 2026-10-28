## Fixes

- Lane writers can no longer hold grants on the shared repository's branch refs and reflogs. Those paths were already read-only under the Seatbelt profile; this removes the leftover grant bookkeeping. The rendered profile is unchanged, but the lane Git grant digest changes, so stored authorities from earlier releases no longer match.
