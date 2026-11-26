use crate::{
    domain::resource::{Data, Resource},
    error::{Error, Result},
};

pub fn parse_change_id(value: &str) -> std::result::Result<String, String> {
    if value.is_empty()
        || value.len() > 128
        || matches!(value, "." | "..")
        || !value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
    {
        return Err(
            "change-id 只允许不超过 128 字符的字母、数字、点、下划线和连字符，且不能为目录跳转"
                .into(),
        );
    }
    Ok(value.into())
}

pub fn matches_resource(resource: &Resource, key: &str) -> bool {
    resource.id == key
        || resource.matches_name(key)
        || matches!(&resource.data,Data::Capability {native_id,..} if native_id.split_once('@').is_some_and(|(short,_)|short==key))
}

pub fn revision(value: &str) -> Result<String> {
    if value.len() != 40 || !value.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(Error::new(
            "invalid_revision",
            "revision 必须是完整的 40 位提交 SHA",
        ));
    }
    Ok(value.to_ascii_lowercase())
}

#[cfg(test)]
#[path = "../../../tests/unit/skills_policy.rs"]
mod tests;
