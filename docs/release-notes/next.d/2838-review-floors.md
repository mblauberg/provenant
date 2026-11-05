## Behaviour changes
### Pipelines and graphs

- Review path floors raise review depth and add required lenses for protected and unknown paths, including protected Markdown paths. Projects can override the bundled `review.floors` policy; explain output reports applied or pending floors. When changed paths cannot be read, reviews apply the highest configured floor and report why (#2838).
