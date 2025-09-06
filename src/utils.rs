pub(crate) fn align_str(origin: &str, width: usize) -> String { 
    let last: String = origin.chars()
        .rev()
        .take(width)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect::<String>();
    let padding = width - last.chars().count();
    format!("{}{}", " ".repeat(padding), last)
}