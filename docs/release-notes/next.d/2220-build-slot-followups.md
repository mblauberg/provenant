## Behaviour changes
### Lanes and settlement

- Build slot files now live under `<state>/build-slots/`, beside `fabric/` rather than inside `fabric/locks/`. A writer lane creates its landing locks under `fabric/locks`, so its state grant covered the slot files and it could unlink or replace one; the host build throttle is now out of every lane's write reach (#2220).
- Provenant refuses to provision a build slot directory owned by another user, since only its owner may repair it, and recreates a slot file another user owns (#2220).
