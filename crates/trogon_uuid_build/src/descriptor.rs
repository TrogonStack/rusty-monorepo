use prost_reflect::{Cardinality, DescriptorPool, DynamicMessage, ExtensionDescriptor, Kind, ReflectMessage, Value};
use trogon_uuid::{Namespace, Uuid};

use crate::{BuildError, Template};

pub(crate) struct Schema {
    enum_option: ExtensionDescriptor,
    value_option: ExtensionDescriptor,
}

pub(crate) struct Recipe {
    pub selection: Template,
    pub namespace: Uuid,
    pub template: String,
}

impl Schema {
    pub fn new(pool: &DescriptorPool) -> Result<Self, BuildError> {
        let namespace = "trogon.uuid.v1.Namespace";
        let format = "trogon.uuid.v1.EnumValueOptions.Format";
        for (message, name, number, message_type) in [
            ("trogon.uuid.v1.EnumOptions", "namespace", 1, Some(namespace)),
            ("trogon.uuid.v1.EnumValueOptions", "format", 1, Some(format)),
            (format, "namespace", 1, Some(namespace)),
            (format, "template", 2, None),
            (namespace, "uuid", 1, None),
            (namespace, "dns", 2, None),
            (namespace, "url", 3, None),
        ] {
            let field = pool
                .get_message_by_name(message)
                .and_then(|message| message.get_field_by_name(name))
                .ok_or_else(|| BuildError::Configuration(format!("UUID option schema is missing {message}.{name}")))?;
            let correct_kind = match (message_type, field.kind()) {
                (Some(expected), Kind::Message(actual)) => actual.full_name() == expected,
                (None, Kind::String) => true,
                _ => false,
            };
            let correct_oneof =
                message != namespace || field.containing_oneof().is_some_and(|oneof| oneof.name() == "value");
            if field.number() != number
                || field.cardinality() != Cardinality::Optional
                || !correct_kind
                || !correct_oneof
            {
                return Err(BuildError::Configuration(format!(
                    "field {message}.{name} does not match the supported UUID option schema"
                )));
            }
        }
        Ok(Self {
            enum_option: extension(
                pool,
                "trogon.uuid.v1.enum",
                870_001,
                "google.protobuf.EnumOptions",
                "trogon.uuid.v1.EnumOptions",
            )?,
            value_option: extension(
                pool,
                "trogon.uuid.v1.enum_value",
                870_002,
                "google.protobuf.EnumValueOptions",
                "trogon.uuid.v1.EnumValueOptions",
            )?,
        })
    }

    pub fn resolve(&self, pool: &DescriptorPool, selection: &Template) -> Result<Recipe, BuildError> {
        let enum_name = selection.enum_name.strip_prefix('.').unwrap_or(&selection.enum_name);
        let enumeration = pool
            .get_enum_by_name(enum_name)
            .ok_or_else(|| selection.error("selected enum does not exist"))?;
        let value = enumeration
            .get_value_by_name(&selection.value_name)
            .ok_or_else(|| selection.error("selected enum value does not exist"))?;
        let options = value.options();
        let value_options = option_message(&options, &self.value_option)
            .ok_or_else(|| selection.error("selected enum value has no UUID format option"))?;
        let format = message_field(&value_options, "format", selection)?
            .ok_or_else(|| selection.error("selected enum value has no UUID format"))?;
        let namespace = if let Some(namespace) = message_field(&format, "namespace", selection)? {
            namespace
        } else {
            let options = enumeration.options();
            let enum_options = option_message(&options, &self.enum_option)
                .ok_or_else(|| selection.error("no namespace is configured on the value or enum"))?;
            message_field(&enum_options, "namespace", selection)?
                .ok_or_else(|| selection.error("no namespace is configured on the value or enum"))?
        };
        let namespace = resolve_namespace(&namespace, selection)?;
        let version_namespace = trogon_uuid::uuid5(&namespace, value.name().as_bytes());
        let template = string_field(&format, "template", selection)?;
        Ok(Recipe {
            selection: selection.clone(),
            namespace: version_namespace,
            template,
        })
    }
}

fn extension(
    pool: &DescriptorPool,
    name: &str,
    number: u32,
    extendee: &str,
    message_name: &str,
) -> Result<ExtensionDescriptor, BuildError> {
    let extension = pool
        .get_extension_by_name(name)
        .ok_or_else(|| BuildError::Configuration(format!("descriptor set must include extension {name}")))?;
    let correct_message = matches!(extension.kind(), Kind::Message(message) if message.full_name() == message_name);
    if extension.number() != number
        || extension.containing_message().full_name() != extendee
        || extension.cardinality() != Cardinality::Optional
        || !correct_message
    {
        return Err(BuildError::Configuration(format!(
            "extension {name} does not match the supported UUID option schema"
        )));
    }
    Ok(extension)
}

fn option_message(options: &DynamicMessage, extension: &ExtensionDescriptor) -> Option<DynamicMessage> {
    if !options.has_extension(extension) {
        return None;
    }
    match options.get_extension(extension).as_ref() {
        Value::Message(message) => Some(message.clone()),
        _ => None,
    }
}

fn message_field(
    message: &DynamicMessage,
    name: &str,
    selection: &Template,
) -> Result<Option<DynamicMessage>, BuildError> {
    let field = message
        .descriptor()
        .get_field_by_name(name)
        .ok_or_else(|| selection.error(format!("UUID option schema is missing {name}")))?;
    if !matches!(field.kind(), Kind::Message(_)) || field.is_list() {
        return Err(selection.error(format!("UUID option {name} must be a message")));
    }
    if !message.has_field(&field) {
        return Ok(None);
    }
    match message.get_field(&field).as_ref() {
        Value::Message(value) => Ok(Some(value.clone())),
        _ => Err(selection.error(format!("UUID option {name} must be a message"))),
    }
}

fn string_field(message: &DynamicMessage, name: &str, selection: &Template) -> Result<String, BuildError> {
    let field = message
        .descriptor()
        .get_field_by_name(name)
        .ok_or_else(|| selection.error(format!("UUID option schema is missing {name}")))?;
    if !matches!(field.kind(), Kind::String) || field.is_list() {
        return Err(selection.error(format!("UUID option {name} must be a string")));
    }
    match message.get_field(&field).as_ref() {
        Value::String(value) => Ok(value.clone()),
        _ => Err(selection.error(format!("UUID option {name} must be a string"))),
    }
}

fn resolve_namespace(message: &DynamicMessage, selection: &Template) -> Result<Uuid, BuildError> {
    let descriptor = message.descriptor();
    let oneof = descriptor
        .oneofs()
        .find(|oneof| oneof.name() == "value")
        .ok_or_else(|| selection.error("namespace schema is missing its value oneof"))?;
    let fields: Vec<_> = oneof.fields().filter(|field| message.has_field(field)).collect();
    if fields.len() != 1 {
        return Err(selection.error("namespace must select exactly one of uuid, dns, or url"));
    }
    let field = fields[0].name();
    let name = string_field(message, field, selection)?;
    match field {
        "uuid" => {
            let namespace = Uuid::parse_str(&name).map_err(|source| BuildError::NamespaceUuid {
                target: selection.target(),
                source: Box::new(source),
            })?;
            Ok(namespace)
        }
        "dns" => Ok(Namespace::Dns(&name).resolve()),
        "url" => Ok(Namespace::Url(&name).resolve()),
        _ => Err(selection.error("unsupported namespace kind")),
    }
}
