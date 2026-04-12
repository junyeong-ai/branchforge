#![allow(missing_docs)]

pub const NAMESPACE_SEP: char = ':';

pub fn namespaced(plugin: &str, resource: &str) -> String {
    format!("{}{}{}", plugin, NAMESPACE_SEP, resource)
}

pub fn parse(name: &str) -> Option<(&str, &str)> {
    name.split_once(NAMESPACE_SEP)
}

pub fn is_namespaced(name: &str) -> bool {
    name.contains(NAMESPACE_SEP)
}

pub fn plugin_name(name: &str) -> Option<&str> {
    parse(name).map(|(p, _)| p)
}

pub fn resource_name(name: &str) -> Option<&str> {
    parse(name).map(|(_, r)| r)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_namespaced() {
        assert_eq!(namespaced("my-plugin", "commit"), "my-plugin:commit");
        assert_eq!(namespaced("org", "tool"), "org:tool");
    }

    #[test]
    fn test_parse() {
        assert_eq!(parse("my-plugin:commit"), Some(("my-plugin", "commit")));
        assert_eq!(parse("no-namespace"), None);
        assert_eq!(parse("a:b:c"), Some(("a", "b:c")));
    }

    #[test]
    fn test_is_namespaced() {
        assert!(is_namespaced("plugin:resource"));
        assert!(!is_namespaced("plain"));
    }

    #[test]
    fn test_plugin_name() {
        assert_eq!(plugin_name("my-plugin:skill"), Some("my-plugin"));
        assert_eq!(plugin_name("plain"), None);
    }

    #[test]
    fn test_resource_name() {
        assert_eq!(resource_name("my-plugin:skill"), Some("skill"));
        assert_eq!(resource_name("plain"), None);
    }
}
