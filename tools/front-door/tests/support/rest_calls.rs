// The fake records argv without the executable. Restrict landing to repository
// REST endpoints, including when gh options precede the endpoint.
pub(crate) fn rest_only(calls: &str) -> bool {
    calls.lines().all(|line| {
        let mut words = line.split_whitespace();
        if words.next() != Some("api") {
            return false;
        }
        let mut endpoint = None;
        while let Some(word) = words.next() {
            match word {
                "--include" | "--paginate" | "--slurp" => {}
                "--method" | "-X" | "-f" | "-F" | "-H" | "--header" => {
                    if words.next().is_none() {
                        return false;
                    }
                }
                _ if endpoint.is_none()
                    && word.strip_prefix("repos/").is_some_and(|path| {
                        let mut parts = path.split('/');
                        parts.next().is_some_and(|owner| !owner.is_empty())
                            && parts.next().is_some_and(|repo| !repo.is_empty())
                    }) =>
                {
                    endpoint = Some(word)
                }
                _ => return false,
            }
        }
        endpoint.is_some()
    })
}
