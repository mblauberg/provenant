## Changes
### Fabric and graphs

- Steering a lane that has just settled no longer refuses with `steer_target_refused`: by default the note is recorded for the lane's next follow-up and the follow-up is named in the output, while `lanes.steer_settled=warn` warns with the sourced setting and exits 0 without effect. (#3809)
