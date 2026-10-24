//! Text shown in the interface.

use lookout_core::model::Address;

/// Up to two initials for an avatar.
pub fn initials(name: &str) -> String {
    // For an address, the part before the domain.
    let name = name.split('@').next().unwrap_or(name);
    let words: Vec<&str> = name
        .split(|c: char| c.is_whitespace() || c == '@' || c == '.' || c == ',')
        .filter(|word| word.chars().next().is_some_and(char::is_alphanumeric))
        .collect();
    let mut initials: String = match words.as_slice() {
        [] => String::new(),
        [only] => only.chars().take(1).collect(),
        [first, .., last] => first.chars().take(1).chain(last.chars().take(1)).collect(),
    };
    initials = initials.to_uppercase();
    if initials.is_empty() { "?".into() } else { initials }
}

pub fn size(bytes: u64) -> String {
    const KB: f64 = 1024.0;
    let bytes = bytes as f64;
    if bytes < KB {
        format!("{bytes} B")
    } else if bytes < KB * KB {
        format!("{:.0} KB", bytes / KB)
    } else {
        format!("{:.1} MB", bytes / KB / KB)
    }
}

/// `Name <address>`, or the address alone.
pub fn address(address: &Address) -> String {
    if address.name.trim().is_empty() || address.name == address.address {
        address.address.clone()
    } else {
        format!("{} <{}>", address.name, address.address)
    }
}

pub fn addresses(list: &[Address]) -> String {
    list.iter().map(address).collect::<Vec<_>>().join("; ")
}

/// Names only, for the reading pane's header.
pub fn names(list: &[Address]) -> String {
    list.iter().map(|address| address.display().to_owned()).collect::<Vec<_>>().join("; ")
}

/// Reads what the user typed in a recipient field: addresses separated by
/// `;` or `,`, each bare or as `Name <address>`. Returns the ones that
/// don't look like addresses separately.
pub fn parse_addresses(text: &str) -> (Vec<Address>, Vec<String>) {
    let mut found = Vec::new();
    let mut invalid = Vec::new();
    for part in text.split([';', ',']) {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }
        let (name, address) = match (part.rfind('<'), part.rfind('>')) {
            (Some(open), Some(close)) if open < close => {
                (part[..open].trim().trim_matches('"').to_owned(), part[open + 1..close].trim().to_owned())
            }
            _ => (String::new(), part.to_owned()),
        };
        let valid = address.contains('@')
            && !address.contains(char::is_whitespace)
            && !address.starts_with('@')
            && !address.ends_with('@');
        if valid {
            found.push(Address { name, address });
        } else {
            invalid.push(part.to_owned());
        }
    }
    (found, invalid)
}

/// Replaces the address being typed (after the last separator) with `picked`.
pub fn complete_address(current: &str, picked: &str) -> String {
    let kept = match current.rfind([';', ',']) {
        Some(at) => current[..=at].to_owned() + " ",
        None => String::new(),
    };
    format!("{kept}{picked}; ")
}

/// The address being typed in a recipient field.
pub fn typed_address(current: &str) -> &str {
    match current.rfind([';', ',']) {
        Some(at) => current[at + 1..].trim(),
        None => current.trim(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn initials_come_from_the_first_and_last_names() {
        assert_eq!(initials("Ana María Pérez"), "AP");
        assert_eq!(initials("bob.smith@example.com"), "BS");
        assert_eq!(initials("Zoë"), "Z");
        assert_eq!(initials(""), "?");
    }

    #[test]
    fn addresses_parse_with_or_without_names() {
        let (found, invalid) = parse_addresses("Ana <ana@x.test>; bo@x.test, nonsense; ");
        assert_eq!(found, [
            Address { name: "Ana".into(), address: "ana@x.test".into() },
            Address { name: String::new(), address: "bo@x.test".into() },
        ]);
        assert_eq!(invalid, ["nonsense"]);
    }

    #[test]
    fn completion_replaces_the_last_address() {
        assert_eq!(complete_address("a@x.test; an", "Ana <ana@x.test>"), "a@x.test; Ana <ana@x.test>; ");
        assert_eq!(complete_address("an", "Ana <ana@x.test>"), "Ana <ana@x.test>; ");
        assert_eq!(typed_address("a@x.test; an"), "an");
    }
}
