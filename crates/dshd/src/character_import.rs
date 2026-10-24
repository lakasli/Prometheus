//! Independent, bounded text-only importer. It does not execute AIRI code or install resources.
use crate::character::{self, CharacterError, CharacterResult};
use axum::http::StatusCode;
use serde::de::{DeserializeSeed, Error, MapAccess, SeqAccess, Visitor};
use serde::Serialize;
use serde_json::{json, Map, Number, Value};
use std::{
    collections::HashSet,
    io::{Cursor, Read},
};

pub const JSON_LIMIT: usize = 256 * 1024;
pub const ZIP_LIMIT: usize = 8 * 1024 * 1024;
pub const CONVERTER_VERSION: &str = "dsh-airi-text-v1";
#[derive(Clone, Debug, Serialize)]
pub struct ImportReport {
    pub card: Value,
    pub warnings: Vec<Value>,
    pub dropped_fields: Vec<String>,
    pub issues: Vec<Value>,
    pub digest: String,
    pub input_format: String,
    pub converter_version: String,
}
fn invalid(message: &str) -> CharacterError {
    CharacterError::new("invalid_character_import", message)
}
fn unsupported() -> CharacterError {
    CharacterError {
        code: "unsupported_card_format",
        message: "仅支持CCv3 3.0 JSON或AIRI v1分享ZIP".into(),
        status: StatusCode::UNSUPPORTED_MEDIA_TYPE,
    }
}
fn too_large() -> CharacterError {
    CharacterError {
        code: "character_import_too_large",
        message: "角色卡文件或解压资源超过限制".into(),
        status: StatusCode::PAYLOAD_TOO_LARGE,
    }
}
struct Seed(usize);
impl<'de> DeserializeSeed<'de> for Seed {
    type Value = Value;
    fn deserialize<D: serde::Deserializer<'de>>(self, d: D) -> Result<Value, D::Error> {
        if self.0 > 32 {
            return Err(D::Error::custom("JSON nesting exceeds 32"));
        }
        d.deserialize_any(Node(self.0))
    }
}
struct Node(usize);
impl<'de> Visitor<'de> for Node {
    type Value = Value;
    fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("bounded JSON")
    }
    fn visit_bool<E: Error>(self, v: bool) -> Result<Value, E> {
        Ok(Value::Bool(v))
    }
    fn visit_i64<E: Error>(self, v: i64) -> Result<Value, E> {
        Ok(Value::Number(v.into()))
    }
    fn visit_u64<E: Error>(self, v: u64) -> Result<Value, E> {
        Ok(Value::Number(v.into()))
    }
    fn visit_f64<E: Error>(self, v: f64) -> Result<Value, E> {
        Number::from_f64(v)
            .map(Value::Number)
            .ok_or_else(|| E::custom("non-finite JSON number"))
    }
    fn visit_str<E: Error>(self, v: &str) -> Result<Value, E> {
        Ok(Value::String(v.into()))
    }
    fn visit_string<E: Error>(self, v: String) -> Result<Value, E> {
        Ok(Value::String(v))
    }
    fn visit_unit<E: Error>(self) -> Result<Value, E> {
        Ok(Value::Null)
    }
    fn visit_seq<A: SeqAccess<'de>>(self, mut access: A) -> Result<Value, A::Error> {
        let mut values = vec![];
        while let Some(value) = access.next_element_seed(Seed(self.0 + 1))? {
            if values.len() >= 4096 {
                return Err(A::Error::custom("too many JSON elements"));
            }
            values.push(value);
        }
        Ok(Value::Array(values))
    }
    fn visit_map<A: MapAccess<'de>>(self, mut access: A) -> Result<Value, A::Error> {
        let mut values = Map::new();
        while let Some(key) = access.next_key::<String>()? {
            if values.len() >= 4096 {
                return Err(A::Error::custom("too many JSON fields"));
            }
            if values.contains_key(&key) {
                return Err(A::Error::custom("duplicate JSON key"));
            }
            values.insert(key, access.next_value_seed(Seed(self.0 + 1))?);
        }
        Ok(Value::Object(values))
    }
}
pub fn parse_json(bytes: &[u8]) -> CharacterResult<Value> {
    if bytes.len() > JSON_LIMIT {
        return Err(too_large());
    }
    std::str::from_utf8(bytes).map_err(|_| invalid("文件必须是UTF-8 JSON"))?;
    let mut de = serde_json::Deserializer::from_slice(bytes);
    let value = Seed(0)
        .deserialize(&mut de)
        .map_err(|_| invalid("JSON无效、键重复或嵌套超过32层"))?;
    de.end().map_err(|_| invalid("JSON文件含多余内容"))?;
    Ok(value)
}
fn secret_fields(value: &Value) -> bool {
    match value {
        Value::Object(object) => object.iter().any(|(key, value)| {
            let key = key.to_ascii_lowercase().replace(['_', '-'], "");
            [
                "apikey",
                "token",
                "accesstoken",
                "refreshtoken",
                "appsecret",
                "clientsecret",
                "password",
                "credentialref",
                "endpoint",
                "baseurl",
                "authorization",
            ]
            .contains(&key.as_str())
                || secret_fields(value)
        }),
        Value::Array(items) => items.iter().any(secret_fields),
        _ => false,
    }
}
// ZipArchive may collapse identically named central-directory entries. Validate the
// original standard ZIP records before letting that library construct its index.
fn little_u16(bytes: &[u8], offset: usize) -> CharacterResult<u16> {
    let data = bytes
        .get(offset..offset + 2)
        .ok_or_else(|| invalid("ZIP记录被截断"))?;
    Ok(u16::from_le_bytes([data[0], data[1]]))
}
fn little_u32(bytes: &[u8], offset: usize) -> CharacterResult<u32> {
    let data = bytes
        .get(offset..offset + 4)
        .ok_or_else(|| invalid("ZIP记录被截断"))?;
    Ok(u32::from_le_bytes([data[0], data[1], data[2], data[3]]))
}
fn unsupported_zip() -> CharacterError {
    CharacterError {
        code: "unsupported_card_format",
        message: "仅支持单卷标准ZIP角色包；ZIP64、多卷或扩展路径编码暂不支持".into(),
        status: StatusCode::UNSUPPORTED_MEDIA_TYPE,
    }
}
fn zip_extra(bytes: &[u8]) -> CharacterResult<()> {
    let mut cursor = 0usize;
    while cursor < bytes.len() {
        let kind = little_u16(bytes, cursor)?;
        let length = usize::from(little_u16(bytes, cursor + 2)?);
        cursor = cursor
            .checked_add(4 + length)
            .ok_or_else(|| invalid("ZIP扩展字段越界"))?;
        if cursor > bytes.len() {
            return Err(invalid("ZIP扩展字段被截断"));
        }
        // No ZIP64, AES encryption or alternate Unicode path interpretation.
        if matches!(kind, 0x0001 | 0x9901 | 0x7075) {
            return Err(unsupported_zip());
        }
    }
    Ok(())
}
fn preflight_zip(bytes: &[u8]) -> CharacterResult<usize> {
    if bytes.len() > ZIP_LIMIT {
        return Err(too_large());
    }
    if bytes.len() < 22 {
        return Err(invalid("ZIP缺少结束目录"));
    }
    let start = bytes.len().saturating_sub(22 + usize::from(u16::MAX));
    let end = (start..=bytes.len() - 22)
        .rev()
        .find(|&offset| {
            bytes.get(offset..offset + 4) == Some(b"PK\x05\x06")
                && little_u16(bytes, offset + 20)
                    .is_ok_and(|length| offset + 22 + usize::from(length) == bytes.len())
        })
        .ok_or_else(|| invalid("ZIP结束目录或尾部注释无效"))?;
    let disk = little_u16(bytes, end + 4)?;
    let directory_disk = little_u16(bytes, end + 6)?;
    let disk_count = little_u16(bytes, end + 8)?;
    let count = little_u16(bytes, end + 10)?;
    let size = little_u32(bytes, end + 12)?;
    let offset = little_u32(bytes, end + 16)?;
    if disk != 0
        || directory_disk != 0
        || disk_count != count
        || count == u16::MAX
        || size == u32::MAX
        || offset == u32::MAX
        || (end >= 20 && bytes.get(end - 20..end - 16) == Some(b"PK\x06\x07"))
    {
        return Err(unsupported_zip());
    }
    if count > 64 {
        return Err(too_large());
    }
    let directory_start = offset as usize;
    let directory_end = directory_start
        .checked_add(size as usize)
        .ok_or_else(|| invalid("ZIP目录范围溢出"))?;
    if directory_end != end || directory_start > bytes.len() {
        return Err(invalid("ZIP目录偏移、长度或尾部不一致"));
    }
    let mut cursor = directory_start;
    let mut names: Vec<(String, bool)> = Vec::new();
    let mut ranges: Vec<(usize, usize)> = Vec::new();
    let mut total = 0u64;
    for _ in 0..count {
        if cursor
            .checked_add(46)
            .is_none_or(|next| next > directory_end)
            || bytes.get(cursor..cursor + 4) != Some(b"PK\x01\x02")
        {
            return Err(invalid("ZIP中央目录条目无效或计数不一致"));
        }
        let flags = little_u16(bytes, cursor + 8)?;
        let method = little_u16(bytes, cursor + 10)?;
        let crc = little_u32(bytes, cursor + 16)?;
        let compressed = little_u32(bytes, cursor + 20)?;
        let uncompressed = little_u32(bytes, cursor + 24)?;
        let name_length = usize::from(little_u16(bytes, cursor + 28)?);
        let extra_length = usize::from(little_u16(bytes, cursor + 30)?);
        let comment_length = usize::from(little_u16(bytes, cursor + 32)?);
        let start_disk = little_u16(bytes, cursor + 34)?;
        let local_offset = little_u32(bytes, cursor + 42)?;
        if start_disk != 0
            || compressed == u32::MAX
            || uncompressed == u32::MAX
            || local_offset == u32::MAX
        {
            return Err(unsupported_zip());
        }
        if flags & 0x2041 != 0 {
            return Err(invalid("ZIP不允许加密条目"));
        }
        if !matches!(method, 0 | 8) {
            return Err(unsupported_zip());
        }
        let next = cursor
            .checked_add(46 + name_length + extra_length + comment_length)
            .ok_or_else(|| invalid("ZIP中央目录越界"))?;
        if next > directory_end {
            return Err(invalid("ZIP中央目录变长字段被截断"));
        }
        let name_bytes = &bytes[cursor + 46..cursor + 46 + name_length];
        let name = std::str::from_utf8(name_bytes).map_err(|_| invalid("ZIP文件名不是UTF-8"))?;
        let normalized = name.trim_end_matches('/').to_ascii_lowercase();
        let is_directory = name.ends_with('/');
        if name.len() > 256
            || !name.is_ascii()
            || name.starts_with('/')
            || name.contains(['\\', ':'])
            || normalized
                .split('/')
                .any(|part| part.is_empty() || part == "." || part == "..")
            || names.iter().any(|(prior, directory)| {
                prior == &normalized
                    || (!directory && normalized.starts_with(&format!("{prior}/")))
                    || (!is_directory && prior.starts_with(&format!("{normalized}/")))
            })
        {
            return Err(invalid("ZIP含不安全、重复或规范化冲突路径"));
        }
        names.push((normalized, is_directory));
        zip_extra(&bytes[cursor + 46 + name_length..cursor + 46 + name_length + extra_length])?;
        total = total
            .checked_add(u64::from(uncompressed))
            .ok_or_else(too_large)?;
        if total > 32 * 1024 * 1024
            || (uncompressed > 0
                && (compressed == 0 || u64::from(uncompressed) > u64::from(compressed) * 100))
        {
            return Err(too_large());
        }
        let local = local_offset as usize;
        if local
            .checked_add(30)
            .is_none_or(|next| next > directory_start)
            || bytes.get(local..local + 4) != Some(b"PK\x03\x04")
        {
            return Err(invalid("ZIP本地文件头偏移无效"));
        }
        let local_name_length = usize::from(little_u16(bytes, local + 26)?);
        let local_extra_length = usize::from(little_u16(bytes, local + 28)?);
        let data_start = local
            .checked_add(30 + local_name_length + local_extra_length)
            .ok_or_else(|| invalid("ZIP本地文件头越界"))?;
        let data_end = data_start
            .checked_add(compressed as usize)
            .ok_or_else(|| invalid("ZIP文件数据越界"))?;
        if data_start > directory_start
            || data_end > directory_start
            || little_u16(bytes, local + 6)? != flags
            || little_u16(bytes, local + 8)? != method
            || bytes.get(local + 30..local + 30 + local_name_length) != Some(name_bytes)
            || ranges
                .iter()
                .any(|(start, finish)| local < *finish && data_end > *start)
        {
            return Err(invalid("ZIP本地头与中央目录不一致或条目重叠"));
        }
        zip_extra(&bytes[local + 30 + local_name_length..data_start])?;
        let local_crc = little_u32(bytes, local + 14)?;
        let local_compressed = little_u32(bytes, local + 18)?;
        let local_uncompressed = little_u32(bytes, local + 22)?;
        if local_compressed == u32::MAX || local_uncompressed == u32::MAX {
            return Err(unsupported_zip());
        }
        if flags & 8 == 0 {
            if local_crc != crc
                || local_compressed != compressed
                || local_uncompressed != uncompressed
            {
                return Err(invalid("ZIP本地大小或CRC声明与目录不一致"));
            }
        } else if (local_crc != 0 && local_crc != crc)
            || (local_compressed != 0 && local_compressed != compressed)
            || (local_uncompressed != 0 && local_uncompressed != uncompressed)
        {
            return Err(invalid("ZIP数据描述符声明不一致"));
        }
        ranges.push((local, data_end));
        cursor = next;
    }
    if cursor != directory_end || (count > 0 && ranges.iter().map(|range| range.0).min() != Some(0))
    {
        return Err(invalid("ZIP目录计数不一致或包含额外前缀"));
    }
    Ok(usize::from(count))
}

fn zip_json(bytes: &[u8]) -> CharacterResult<(Value, Vec<String>)> {
    if bytes.len() > ZIP_LIMIT {
        return Err(too_large());
    }
    let original_count = preflight_zip(bytes)?;
    let mut zip =
        zip::ZipArchive::new(Cursor::new(bytes)).map_err(|_| invalid("ZIP目录无效或不支持"))?;
    if zip.len() != original_count {
        return Err(invalid("ZIP解析后的目录计数发生变化"));
    }
    if zip.len() > 64 {
        return Err(too_large());
    }
    let mut names = HashSet::new();
    let mut total = 0u64;
    let mut resources = vec![];
    for i in 0..zip.len() {
        let file = zip
            .by_index(i)
            .map_err(|_| invalid("ZIP加密或条目无法读取"))?;
        let name =
            std::str::from_utf8(file.name_raw()).map_err(|_| invalid("ZIP文件名不是UTF-8"))?;
        if name.len() > 256
            || !name.is_ascii()
            || name.starts_with('/')
            || name.contains(['\\', ':'])
            || name
                .trim_end_matches('/')
                .split('/')
                .any(|part| part.is_empty() || part == "." || part == "..")
            || !names.insert(name.to_ascii_lowercase())
        {
            return Err(invalid("ZIP含不安全、重复或冲突路径"));
        }
        if file
            .unix_mode()
            .is_some_and(|mode| mode & 0o170000 == 0o120000)
        {
            return Err(invalid("ZIP不允许符号链接"));
        }
        total = total.checked_add(file.size()).ok_or_else(too_large)?;
        if total > 32 * 1024 * 1024
            || (file.size() > 0
                && (file.compressed_size() == 0
                    || file.size() > file.compressed_size().saturating_mul(100)))
        {
            return Err(too_large());
        }
        if name != "manifest.json" && name != "card.json" {
            resources.push(name.to_string());
        }
    }
    fn read(zip: &mut zip::ZipArchive<Cursor<&[u8]>>, name: &str) -> CharacterResult<Vec<u8>> {
        let mut file = zip
            .by_name(name)
            .map_err(|_| invalid("AIRI包缺少manifest.json或card.json"))?;
        if file.size() > JSON_LIMIT as u64 {
            return Err(too_large());
        }
        let mut out = Vec::new();
        file.by_ref()
            .take(JSON_LIMIT as u64 + 1)
            .read_to_end(&mut out)
            .map_err(|_| invalid("ZIP JSON读取失败或CRC不正确"))?;
        if out.len() > JSON_LIMIT {
            return Err(too_large());
        }
        Ok(out)
    }
    let manifest = parse_json(&read(&mut zip, "manifest.json")?)?;
    if manifest["format"] != "airi-character-card"
        || manifest["version"].as_i64() != Some(1)
        || manifest["card"]["path"] != "card.json"
        || manifest["card"]["spec"] != "chara_card_v3"
    {
        return Err(unsupported());
    }
    if secret_fields(&manifest) {
        return Err(CharacterError::new(
            "secret_field_forbidden",
            "分享包清单含已知秘密字段，请移除后导入",
        ));
    }
    let card = parse_json(&read(&mut zip, "card.json")?)?;
    Ok((card, resources))
}
fn warning(warnings: &mut Vec<Value>, field: &str, message: &str) {
    warnings.push(json!({"field":field,"message":message}));
}
fn normalize_module(
    value: &Value,
    module: &str,
    warnings: &mut Vec<Value>,
) -> CharacterResult<Value> {
    let mut parts = vec![];
    for key in ["provider", "model", "voice_id"] {
        match value.get(key) {
            None => parts.push(String::new()),
            Some(Value::String(s)) => parts.push(s.trim().to_string()),
            _ => return Err(invalid("AIRI模块provider/model/voice_id必须是字符串")),
        }
    }
    let (provider, model, voice) = (&parts[0], &parts[1], &parts[2]);
    if provider.is_empty() && model.is_empty() && voice.is_empty() {
        return Ok(json!({"mode":"inherit"}));
    }
    let mut result = json!({"mode":"prefer","provider_hint":provider,"model_hint":model});
    if module == "speech" && !voice.is_empty() {
        result["voice_hint"] = json!(voice);
    }
    if provider.is_empty() || model.is_empty() {
        warning(
            warnings,
            &format!("data.extensions.dsh.modules.{module}"),
            "AIRI模块偏好不完整，请补全或明确改为inherit/disabled后确认",
        );
    }
    Ok(result)
}
pub fn inspect(bytes: &[u8], content_type: &str) -> CharacterResult<ImportReport> {
    let (source, resources, format) = if content_type.split(';').next() == Some("application/zip")
        || bytes.starts_with(b"PK\x03\x04")
    {
        let (value, resources) = zip_json(bytes)?;
        (value, resources, "airi-zip")
    } else if content_type.split(';').next() == Some("application/json")
        || bytes.iter().find(|b| !b.is_ascii_whitespace()) == Some(&b'{')
    {
        (parse_json(bytes)?, vec![], "ccv3-json")
    } else {
        return Err(unsupported());
    };
    if source["spec"] != "chara_card_v3" || source["spec_version"] != "3.0" {
        return Err(unsupported());
    }
    if secret_fields(&source) {
        return Err(CharacterError::new(
            "secret_field_forbidden",
            "角色卡含端点、凭证或已知秘密字段，请先移除后导入",
        ));
    }
    let data = source["data"]
        .as_object()
        .ok_or_else(|| invalid("角色卡data必须是对象"))?;
    let mut card = character::template("", "");
    let mut dropped = vec![];
    let mut warnings = vec![];
    let supported = [
        "name",
        "nickname",
        "description",
        "personality",
        "scenario",
        "first_mes",
        "mes_example",
        "creator_notes",
        "system_prompt",
        "post_history_instructions",
        "alternate_greetings",
        "group_only_greetings",
        "tags",
        "creator",
        "character_version",
    ];
    for (field, value) in data {
        if supported.contains(&field.as_str()) {
            card["data"][field] = value.clone();
        } else if field != "extensions" {
            dropped.push(format!("data.{}", safe_field(field)));
        }
    }
    for field in source.as_object().into_iter().flat_map(|obj| obj.keys()) {
        if !["spec", "spec_version", "data"].contains(&field.as_str()) {
            dropped.push(format!("$.{}", safe_field(field)));
        }
    }
    if let Some(ext) = data.get("extensions") {
        let ext = ext
            .as_object()
            .ok_or_else(|| invalid("extensions必须是对象"))?;
        if let Some(dsh) = ext.get("dsh") {
            card["data"]["extensions"]["dsh"] = dsh.clone();
            if ext.contains_key("airi") {
                warning(
                    &mut warnings,
                    "data.extensions.airi",
                    "同时提供DSH和AIRI偏好，仅采用DSH偏好",
                );
            }
        } else if let Some(airi) = ext.get("airi") {
            if !airi.is_object() {
                return Err(invalid("AIRI扩展必须是对象"));
            }
            let modules = &airi["modules"];
            if !modules.is_null() && !modules.is_object() {
                return Err(invalid("AIRI模块必须是对象"));
            }
            for (input, output) in [
                ("consciousness", "chat"),
                ("vision", "vision"),
                ("speech", "speech"),
            ] {
                let value = &modules[input];
                if !value.is_null() && !value.is_object() {
                    return Err(invalid("AIRI模块偏好必须是对象"));
                }
                card["data"]["extensions"]["dsh"]["modules"][output] =
                    normalize_module(value, output, &mut warnings)?;
            }
            if let Some(airi) = airi.as_object() {
                for field in airi.keys() {
                    if field != "modules" {
                        dropped.push(format!("data.extensions.airi.{}", safe_field(field)));
                    }
                }
            }
            if let Some(modules) = modules.as_object() {
                for (field, value) in modules {
                    if !["consciousness", "vision", "speech"].contains(&field.as_str()) {
                        dropped.push(format!(
                            "data.extensions.airi.modules.{}",
                            safe_field(field)
                        ));
                    } else if let Some(values) = value.as_object() {
                        for key in values.keys() {
                            if !["provider", "model", "voice_id"].contains(&key.as_str()) {
                                dropped.push(format!(
                                    "data.extensions.airi.modules.{field}.{}",
                                    safe_field(key)
                                ));
                            }
                        }
                    }
                }
            }
        }
        for field in ext.keys() {
            if !["airi", "dsh"].contains(&field.as_str()) {
                dropped.push(format!("data.extensions.{}", safe_field(field)));
            }
        }
    }
    if !resources.is_empty() {
        warning(
            &mut warnings,
            "resources",
            "包内资源未加载、未校验内容且不会安装",
        );
        for resource in resources {
            dropped.push(format!("zip:{resource}"));
        }
    }
    if !card["data"]["post_history_instructions"]
        .as_str()
        .unwrap_or("")
        .is_empty()
    {
        warning(
            &mut warnings,
            "data.post_history_instructions",
            "保留文本但当前编译器不执行",
        );
    }
    if !dropped.is_empty() {
        warning(&mut warnings, "$", "本次为有损文本转换，请检查丢弃字段");
    }
    let issues = character::validate(&card);
    Ok(ImportReport {
        card,
        warnings,
        dropped_fields: dropped,
        issues,
        digest: character::digest(bytes),
        input_format: format.into(),
        converter_version: CONVERTER_VERSION.into(),
    })
}
fn safe_field(field: &str) -> String {
    if field.len() <= 64
        && field
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
    {
        field.into()
    } else {
        "<unknown>".into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    fn package(card: &Value, name: &str) -> Vec<u8> {
        let cursor = Cursor::new(Vec::new());
        let mut zip = zip::ZipWriter::new(cursor);
        let options = zip::write::SimpleFileOptions::default();
        zip.start_file("manifest.json", options).unwrap();
        zip.write_all(br#"{"format":"airi-character-card","version":1,"card":{"path":"card.json","spec":"chara_card_v3"}}"#).unwrap();
        zip.start_file(name, options).unwrap();
        zip.write_all(card.to_string().as_bytes()).unwrap();
        zip.finish().unwrap().into_inner()
    }
    #[test]
    fn rejects_null_module_hints() {
        let mut card = character::template("Test", "");
        card["data"]["extensions"] =
            json!({"airi":{"modules":{"consciousness":{"provider":null}}}});
        assert!(inspect(card.to_string().as_bytes(), "application/json").is_err());
    }
    #[test]
    fn rejects_duplicate_keys_and_deep_json() {
        assert!(parse_json(br#"{"a":1,"a":2}"#).is_err());
        let deep = format!("{}0{}", "[".repeat(34), "]".repeat(34));
        assert!(parse_json(deep.as_bytes()).is_err());
    }
    #[test]
    fn rejects_secrets_and_drops_unknown_values() {
        let mut card = character::template("Test", "");
        card["data"]["extensions"]["api_key"] = json!("synthetic");
        assert_eq!(
            inspect(card.to_string().as_bytes(), "application/json")
                .unwrap_err()
                .code,
            "secret_field_forbidden"
        );
        card["data"]["extensions"]
            .as_object_mut()
            .unwrap()
            .remove("api_key");
        card["data"]["extensions"]["untrusted"] = json!({"script":"must not execute"});
        let report = inspect(card.to_string().as_bytes(), "application/json").unwrap();
        assert!(!report.card.to_string().contains("must not execute"));
        assert!(!report.dropped_fields.is_empty());
    }
    // Keep duplicate central records in raw bytes: ZipWriter/ZipArchive can
    // deduplicate names, which would hide the condition this regression checks.
    fn duplicate_central_entry(bytes: &[u8], uppercase_name: bool) -> Vec<u8> {
        let end = bytes.windows(4).rposition(|v| v == b"PK\x05\x06").unwrap();
        let start = little_u32(bytes, end + 16).unwrap() as usize;
        let first_length = 46
            + usize::from(little_u16(bytes, start + 28).unwrap())
            + usize::from(little_u16(bytes, start + 30).unwrap())
            + usize::from(little_u16(bytes, start + 32).unwrap());
        let second = start + first_length;
        let mut duplicate = bytes[second..end].to_vec();
        if uppercase_name {
            let length = usize::from(little_u16(&duplicate, 28).unwrap());
            duplicate[46..46 + length].make_ascii_uppercase();
        }
        let mut out = Vec::new();
        out.extend_from_slice(&bytes[..end]);
        out.extend_from_slice(&duplicate);
        out.extend_from_slice(&bytes[end..]);
        let next_end = end + duplicate.len();
        let count = little_u16(bytes, end + 10).unwrap() + 1;
        out[next_end + 8..next_end + 10].copy_from_slice(&count.to_le_bytes());
        out[next_end + 10..next_end + 12].copy_from_slice(&count.to_le_bytes());
        let size = little_u32(bytes, end + 12).unwrap() + duplicate.len() as u32;
        out[next_end + 12..next_end + 16].copy_from_slice(&size.to_le_bytes());
        out
    }
    #[test]
    fn rejects_original_duplicate_and_normalized_central_directory_records() {
        let bytes = package(&character::template("Test", ""), "card.json");
        for uppercase in [false, true] {
            let duplicate = duplicate_central_entry(&bytes, uppercase);
            let end = duplicate
                .windows(4)
                .rposition(|v| v == b"PK\x05\x06")
                .unwrap();
            assert_eq!(little_u16(&duplicate, end + 10).unwrap(), 3);
            assert_eq!(
                inspect(&duplicate, "application/zip").unwrap_err().code,
                "invalid_character_import"
            );
        }
    }
    #[test]
    fn rejects_inconsistent_central_directory_counts_and_offsets() {
        let bytes = package(&character::template("Test", ""), "card.json");
        let end = bytes.windows(4).rposition(|v| v == b"PK\x05\x06").unwrap();
        let mut count = bytes.clone();
        count[end + 8..end + 10].copy_from_slice(&1u16.to_le_bytes());
        count[end + 10..end + 12].copy_from_slice(&1u16.to_le_bytes());
        assert!(inspect(&count, "application/zip").is_err());
        let mut offset = bytes.clone();
        offset[end + 16..end + 20].copy_from_slice(&0u32.to_le_bytes());
        assert!(inspect(&offset, "application/zip").is_err());
        let mut trailing = bytes.clone();
        trailing.push(0);
        assert!(inspect(&trailing, "application/zip").is_err());
    }
    #[test]
    fn rejects_zip64_multi_volume_and_conflicting_local_headers() {
        let bytes = package(&character::template("Test", ""), "card.json");
        let end = bytes.windows(4).rposition(|v| v == b"PK\x05\x06").unwrap();
        let mut zip64 = bytes.clone();
        zip64[end + 10..end + 12].copy_from_slice(&u16::MAX.to_le_bytes());
        zip64[end + 8..end + 10].copy_from_slice(&u16::MAX.to_le_bytes());
        assert_eq!(
            inspect(&zip64, "application/zip").unwrap_err().status,
            StatusCode::UNSUPPORTED_MEDIA_TYPE
        );
        let mut multi = bytes.clone();
        multi[end + 4..end + 6].copy_from_slice(&1u16.to_le_bytes());
        assert_eq!(
            inspect(&multi, "application/zip").unwrap_err().status,
            StatusCode::UNSUPPORTED_MEDIA_TYPE
        );
        let mut local = bytes.clone();
        local[30] = b'x';
        assert!(inspect(&local, "application/zip").is_err());
    }
    #[test]
    fn package_is_text_only_and_rejects_traversal() {
        let card = character::template("Test", "");
        assert!(inspect(&package(&card, "card.json"), "application/zip").is_ok());
        assert!(inspect(&package(&card, "../card.json"), "application/zip").is_err());
        assert!(inspect(b"not a zip", "application/zip").is_err());
    }
}
