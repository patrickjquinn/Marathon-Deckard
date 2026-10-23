use crate::Error;

pub fn utf16_to_byte(text: &str, offset: usize) -> Result<usize, Error> {
    let mut units = 0;
    for (position, character) in text.char_indices() {
        if units == offset {
            return Ok(position);
        }
        units += character.len_utf16();
        if units > offset {
            return Err(Error::InvalidOffset);
        }
    }
    if units == offset {
        Ok(text.len())
    } else {
        Err(Error::InvalidOffset)
    }
}

pub fn replace(
    text: &str,
    cursor: usize,
    anchor: usize,
    before: usize,
    after: usize,
    inserted: &str,
) -> Result<(String, usize), Error> {
    let total = text.encode_utf16().count();
    if cursor > total || anchor > total {
        return Err(Error::InvalidOffset);
    }
    let mut start = cursor.min(anchor);
    let mut end = cursor.max(anchor);
    if start == end {
        start = start.saturating_sub(before);
        end = end.saturating_add(after).min(total);
    }
    let start_byte = utf16_to_byte(text, start)?;
    let end_byte = utf16_to_byte(text, end)?;
    let mut output = String::with_capacity(text.len() - (end_byte - start_byte) + inserted.len());
    output.push_str(&text[..start_byte]);
    output.push_str(inserted);
    output.push_str(&text[end_byte..]);
    Ok((output, start + inserted.encode_utf16().count()))
}

pub fn truncate(text: &str, max_units: usize) -> String {
    let mut units = 0;
    text.chars()
        .take_while(|character| {
            units += character.len_utf16();
            units <= max_units
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_offsets_inside_surrogate_pairs() {
        assert_eq!(utf16_to_byte("a😀b", 1), Ok(1));
        assert_eq!(utf16_to_byte("a😀b", 2), Err(Error::InvalidOffset));
        assert_eq!(utf16_to_byte("a😀b", 3), Ok(5));
        assert_eq!(utf16_to_byte("a😀b", 4), Ok(6));
        assert!(utf16_to_byte("a😀b", 5).is_err());
    }

    #[test]
    fn edits_use_utf16_and_selection_takes_precedence() {
        assert_eq!(replace("a😀b", 3, 3, 2, 0, "🎉"), Ok(("a🎉b".into(), 3)));
        assert_eq!(replace("a😀b", 3, 1, 99, 99, "z"), Ok(("azb".into(), 2)));
        assert!(replace("a😀b", 3, 3, 1, 0, "").is_err());
        assert!(replace("abc", usize::MAX, 0, 0, 0, "").is_err());
        assert_eq!(
            replace("abc", 1, 1, usize::MAX, usize::MAX, "x"),
            Ok(("x".into(), 1))
        );
    }

    #[test]
    fn truncation_keeps_whole_characters() {
        assert_eq!(truncate("a😀b", 2), "a");
        assert_eq!(truncate("a😀b", 3), "a😀");
        assert_eq!(truncate("😀", 0), "");
    }
}
