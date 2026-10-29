## Fixes
### Pipelines and graphs

- A graph whose worst-case checkpoint could exceed 16 MiB is now refused at submission with the size it could reach, and a planner admission or loop extension that would cross the limit pauses instead. Stored outputs, reasons and usage lists have fixed caps, so an accepted graph can always save and reload its checkpoint (#2044; see [checkpoint envelope](../../specs/fabric.md#checkpoint-envelope)).
