//! GPUI の hover は kind にかかわらず Markdown として描画するため、
//! plaintext と言語付きコードを、内容を解釈しない fenced block に変換する。
use lsp_types::{Hover, HoverContents, MarkedString, MarkupContent, MarkupKind};

fn code_block(language: &str, text: &str) -> String {
    let longest_run = text.split(|ch| ch != '`').map(str::len).max().unwrap_or(0);
    let fence = "`".repeat(3.max(longest_run + 1));
    let language: String = language
        .chars()
        .filter(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-' | '+' | '#' | '.'))
        .collect();
    format!("{fence}{language}\n{text}\n{fence}")
}

fn marked_string(value: MarkedString) -> String {
    match value {
        // LSP の MarkedString::String は Markdown として定義されている。
        MarkedString::String(text) => text,
        MarkedString::LanguageString(code) => code_block(&code.language, &code.value),
    }
}

pub fn for_markdown(mut hover: Hover) -> Hover {
    let value = match hover.contents {
        HoverContents::Scalar(value) => marked_string(value),
        HoverContents::Array(values) => values
            .into_iter()
            .map(marked_string)
            .collect::<Vec<_>>()
            .join("\n\n"),
        HoverContents::Markup(value) => match value.kind {
            MarkupKind::PlainText => code_block("", &value.value),
            MarkupKind::Markdown => value.value,
        },
    };
    hover.contents = HoverContents::Markup(MarkupContent {
        kind: MarkupKind::Markdown,
        value,
    });
    hover
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn literal_content_cannot_close_its_fence_or_inject_language_lines() {
        assert_eq!(
            code_block("rust\n```", "```\n**literal**\n```"),
            "````rust\n```\n**literal**\n```\n````"
        );
    }
}
