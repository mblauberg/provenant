## Fixes

- The Console lists the open requests of every project on the host when opened from Finder or `provenant console open`, each row naming its project, and answers them. Before, `provenant inbox` failed outside a project with "utility operation has an invalid or unsupported value"; it now lists host-wide there, and with no usable state reports `no_project` with a cause and a repair.
- `Provenant Console.app` now has the Provenant icon, compiled from `assets/brand/Provenant.icon` by `bundle` when Xcode is installed.
