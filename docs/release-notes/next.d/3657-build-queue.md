## Behaviour changes

### Lanes and settlement

- Build jobs now use a priority queue with aging. The `build.overflow` default is now `auto`, and the `build.clean` default is now `untracked`. (#3657)
