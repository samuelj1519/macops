use crate::error::{Error, Result};

pub(crate) fn repository_id(value: &str) -> Result<&str> {
    let part = |name: &str| {
        name.len() <= 100
            && name
                .as_bytes()
                .first()
                .is_some_and(u8::is_ascii_alphanumeric)
            && name
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || b"._-".contains(&byte))
    };
    if value
        .split_once('/')
        .is_some_and(|(owner, repo)| part(owner) && part(repo))
    {
        Ok(value)
    } else {
        Err(Error::new(
            "recommend_repository_invalid",
            "GitHub 仓库需要 OWNER/REPO",
        ))
    }
}

pub(crate) fn repository_subject(value: &str) -> Option<&str> {
    let value = value.strip_prefix("https://github.com/").unwrap_or(value);
    let value = value.trim_end_matches('/');
    repository_id(value.strip_suffix(".git").unwrap_or(value)).ok()
}

pub(crate) fn domain_link(value: &str) -> bool {
    let Some(url) = value.strip_prefix("https://") else {
        return false;
    };
    if value.len() > 2048
        || !value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b".-_/:%?=&#~".contains(&b))
    {
        return false;
    }
    let host = url.split(['/', '?', '#']).next().unwrap_or_default();
    let Some((prefix, tld)) = host.rsplit_once('.') else {
        return false;
    };
    !prefix.is_empty()
        && !tld.is_empty()
        && tld.bytes().all(|b| b.is_ascii_alphabetic())
        && host.split('.').all(|label| {
            !label.is_empty()
                && !label.starts_with('-')
                && !label.ends_with('-')
                && label
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'-')
        })
}
