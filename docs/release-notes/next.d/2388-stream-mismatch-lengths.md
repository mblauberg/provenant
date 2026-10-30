## Fixes

- A streamed result that changes length during its final check now names the recorded length and the length observed on disk, and the stream check, instead of only saying it changed while it was read. The refusal kind and its state-corrupt classification are unchanged, and no artifact content is named ([#2388](https://github.com/mblauberg/provenant/issues/2388)).
