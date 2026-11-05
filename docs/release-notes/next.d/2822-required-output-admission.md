## Behaviour changes
### Pipelines and graphs

- Schema 2 graphs can require selected commit, verdict and findings outputs from direct sources, or complete composite findings. The consumer stops before launch when the value is unavailable; unsupported selections are refused at admission (#2822).
