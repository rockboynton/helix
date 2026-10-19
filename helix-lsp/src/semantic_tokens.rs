//! Semantic token negotiation and decoding, independent of the UI and theme.
use crate::{
    lsp,
    util::{lsp_pos_to_pos, pos_to_lsp_pos},
    OffsetEncoding,
};
use helix_core::Rope;
use std::ops::Range;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Highlight {
    pub range: Range<usize>,
    pub scope: &'static str,
}

/// Map protocol types to existing Helix theme scopes. Unknown types leave
/// Tree-sitter untouched rather than painting identifiers a generic color.
fn scope(kind: &str, modifiers: u32, legend: &lsp::SemanticTokensLegend) -> Option<&'static str> {
    let has = |name| {
        legend
            .token_modifiers
            .iter()
            .enumerate()
            .any(|(i, modifier)| i < 32 && modifiers & (1 << i) != 0 && modifier.as_str() == name)
    };
    Some(match kind {
        "namespace" => "namespace",
        "type" | "class" | "struct" | "enum" | "interface" | "union" | "typeAlias" => {
            if has("defaultLibrary") {
                "type.builtin"
            } else {
                "type"
            }
        }
        "typeParameter" => "type.parameter",
        "parameter" => "variable.parameter",
        "variable" if has("readonly") => "constant",
        "variable" if has("defaultLibrary") => "variable.builtin",
        "variable" if has("mutable") => "variable.mutable",
        "variable" => "variable",
        "property" => "variable.other.member",
        "enumMember" => "type.enum.variant",
        "function" if has("defaultLibrary") => "function.builtin",
        "function" => "function",
        "method" => "function.method",
        "macro" => "function.macro",
        "keyword" => "keyword",
        "modifier" => "keyword.storage.modifier",
        "comment" if has("documentation") => "comment.block.documentation",
        "comment" => "comment",
        "string" => "string",
        "number" => "constant.numeric",
        "regexp" => "string.regexp",
        "operator" => "operator",
        "decorator" | "attribute" => "attribute",
        "builtinType" => "type.builtin",
        "constParameter" => "constant",
        "selfKeyword" => "variable.builtin",
        "selfTypeKeyword" => "type.builtin",
        "lifetime" => "label",
        "boolean" => "constant.builtin.boolean",
        "character" => "constant.character",
        "escapeSequence" => "constant.character.escape",
        "formatSpecifier" => "special",
        "punctuation" => "punctuation",
        "brace" | "bracket" | "parenthesis" | "angle" => "punctuation.bracket",
        "comma" | "semicolon" | "colon" | "dot" => "punctuation.delimiter",
        _ => return None,
    })
}

pub fn capabilities() -> lsp::SemanticTokensClientCapabilities {
    lsp::SemanticTokensClientCapabilities {
        dynamic_registration: Some(false),
        requests: lsp::SemanticTokensClientCapabilitiesRequests {
            range: Some(true),
            full: Some(lsp::SemanticTokensFullOptions::Bool(true)),
        },
        token_types: [
            "namespace",
            "type",
            "class",
            "enum",
            "interface",
            "struct",
            "typeParameter",
            "parameter",
            "variable",
            "property",
            "enumMember",
            "function",
            "method",
            "macro",
            "keyword",
            "modifier",
            "comment",
            "string",
            "number",
            "regexp",
            "operator",
            "decorator",
            "union",
            "typeAlias",
            "builtinType",
            "constParameter",
            "selfKeyword",
            "selfTypeKeyword",
            "lifetime",
            "boolean",
            "character",
            "escapeSequence",
            "formatSpecifier",
            "attribute",
            "punctuation",
            "brace",
            "bracket",
            "parenthesis",
            "angle",
            "comma",
            "semicolon",
            "colon",
            "dot",
        ]
        .into_iter()
        .map(lsp::SemanticTokenType::new)
        .collect(),
        token_modifiers: [
            "declaration",
            "definition",
            "readonly",
            "static",
            "deprecated",
            "abstract",
            "async",
            "modification",
            "documentation",
            "defaultLibrary",
            "mutable",
        ]
        .into_iter()
        .map(lsp::SemanticTokenModifier::new)
        .collect(),
        formats: vec![lsp::TokenFormat::RELATIVE],
        overlapping_token_support: Some(false),
        multiline_token_support: Some(false),
        server_cancel_support: Some(false),
        augments_syntax_tokens: Some(true),
    }
}

pub fn options(capabilities: &lsp::ServerCapabilities) -> Option<&lsp::SemanticTokensOptions> {
    match capabilities.semantic_tokens_provider.as_ref()? {
        lsp::SemanticTokensServerCapabilities::SemanticTokensOptions(options) => Some(options),
        lsp::SemanticTokensServerCapabilities::SemanticTokensRegistrationOptions(options) => {
            Some(&options.semantic_tokens_options)
        }
    }
}

/// Decode relative positions against the exact text snapshot that was sent.
/// Invalid, overlapping or multiline tokens are discarded defensively.
pub fn decode(
    text: &Rope,
    tokens: &[lsp::SemanticToken],
    legend: &lsp::SemanticTokensLegend,
    encoding: OffsetEncoding,
) -> Vec<Highlight> {
    let mut result: Vec<Highlight> = Vec::with_capacity(tokens.len());
    let (mut line, mut column) = (0u32, 0u32);
    for token in tokens {
        let Some(next_line) = line.checked_add(token.delta_line) else {
            break;
        };
        let Some(next_column) =
            (if token.delta_line == 0 { column } else { 0 }).checked_add(token.delta_start)
        else {
            break;
        };
        line = next_line;
        column = next_column;
        let Some(end_column) = column.checked_add(token.length) else {
            continue;
        };
        let Some(kind) = legend.token_types.get(token.token_type as usize) else {
            continue;
        };
        let Some(scope) = scope(kind.as_str(), token.token_modifiers_bitset, legend) else {
            continue;
        };
        let Some(start) = lsp_pos_to_pos(text, lsp::Position::new(line, column), encoding) else {
            continue;
        };
        let Some(end) = lsp_pos_to_pos(text, lsp::Position::new(line, end_column), encoding) else {
            continue;
        };
        if start >= end
            || text.char_to_line(start) != line as usize
            || text.char_to_line(end - 1) != line as usize
            || result
                .last()
                .is_some_and(|previous| previous.range.end > start)
            || pos_to_lsp_pos(text, start, encoding) != lsp::Position::new(line, column)
            || pos_to_lsp_pos(text, end, encoding) != lsp::Position::new(line, end_column)
        {
            continue;
        }
        result.push(Highlight {
            range: start..end,
            scope,
        });
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    fn token(line: u32, start: u32, length: u32, kind: u32) -> lsp::SemanticToken {
        lsp::SemanticToken {
            delta_line: line,
            delta_start: start,
            length,
            token_type: kind,
            token_modifiers_bitset: 0,
        }
    }
    #[test]
    fn unicode_offsets_line_resets_and_legend_indices() {
        let text = Rope::from_str("😀 café\nfoo bar\n");
        let legend = lsp::SemanticTokensLegend {
            token_types: vec!["function".into(), "type".into()],
            token_modifiers: vec![],
        };
        for (encoding, column, length) in [
            (OffsetEncoding::Utf16, 3, 4),
            (OffsetEncoding::Utf8, 5, 5),
            (OffsetEncoding::Utf32, 2, 4),
        ] {
            let ranges = decode(
                &text,
                &[token(0, column, length, 0), token(1, 4, 3, 1)],
                &legend,
                encoding,
            );
            assert_eq!(
                ranges
                    .iter()
                    .map(|span| text.slice(span.range.clone()).to_string())
                    .collect::<Vec<_>>(),
                ["café", "bar"]
            );
            assert_eq!(ranges[1].scope, "type");
        }
    }
    #[test]
    fn rejects_bad_types_overlaps_overflow_and_out_of_bounds() {
        let text = Rope::from_str("abc def\n");
        let legend = lsp::SemanticTokensLegend {
            token_types: vec!["variable".into()],
            token_modifiers: vec![],
        };
        let tokens = [
            token(0, 0, 3, 0),
            token(0, 1, 2, 0),
            token(0, 3, 3, 99),
            token(0, 0, 0, 0),
            token(2, 0, 3, 0),
            token(u32::MAX, 0, 1, 0),
        ];
        assert_eq!(
            decode(&text, &tokens, &legend, OffsetEncoding::Utf16),
            vec![Highlight {
                range: 0..3,
                scope: "variable"
            }]
        );
    }
    #[test]
    fn maps_standard_and_rust_analyzer_modifiers() {
        let legend = lsp::SemanticTokensLegend {
            token_types: vec![],
            token_modifiers: vec!["readonly".into(), "mutable".into(), "defaultLibrary".into()],
        };
        assert_eq!(scope("variable", 1, &legend), Some("constant"));
        assert_eq!(scope("variable", 2, &legend), Some("variable.mutable"));
        assert_eq!(scope("type", 4, &legend), Some("type.builtin"));
        assert_eq!(scope("not-a-token-type", 0, &legend), None);
    }
}
