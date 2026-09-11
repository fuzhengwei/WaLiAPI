//! PDF 字形修复与 FTS 检索投影。原文/代码和检索文本各自保留语义。

use unicode_normalization::UnicodeNormalization;

/// PDF 文字层可能把汉字输出为康熙部首。仅折叠部首，不改写代码示例中的
/// 全角字符串、圈号和上标（整段 NFKC 会改变这些字面值）。
pub fn normalize_radicals(text: &str) -> String {
    let mut result = String::with_capacity(text.len());
    for ch in text.chars() {
        if matches!(ch, '\u{2e80}'..='\u{2fff}') {
            result.extend(ch.to_string().nfkc());
        } else {
            result.push(ch);
        }
    }
    result
}

fn is_cjk(ch: char) -> bool {
    matches!(ch, '\u{3400}'..='\u{4dbf}' | '\u{4e00}'..='\u{9fff}' |
        '\u{f900}'..='\u{faff}' | '\u{20000}'..='\u{323af}')
}

/// 文档与查询共用分词；中文连续文本使用重叠双字词，兼容 unicode61。
/// 文档额外保存单字词，供单字查询使用；查询不扩散为大量单字 OR。
fn tokens(text: &str, index: bool) -> Vec<String> {
    let normalized: Vec<char> = text.nfkc().collect();
    let mut tokens = Vec::new();
    let mut start = 0;
    while start < normalized.len() {
        let cjk = is_cjk(normalized[start]);
        if !cjk && !normalized[start].is_alphanumeric() && normalized[start] != '_' {
            start += 1;
            continue;
        }
        let mut end = start + 1;
        while end < normalized.len()
            && is_cjk(normalized[end]) == cjk
            && (normalized[end].is_alphanumeric() || normalized[end] == '_')
        {
            end += 1;
        }
        let run = &normalized[start..end];
        if cjk {
            if index || run.len() == 1 {
                tokens.extend(run.iter().map(char::to_string));
            }
            tokens.extend(run.windows(2).map(|pair| pair.iter().collect()));
        } else {
            tokens.push(run.iter().collect());
        }
        start = end;
    }
    tokens
}

pub fn search_projection(text: &str) -> String {
    tokens(text, true).join(" ")
}

pub fn query_tokens(text: &str) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    tokens(text, false)
        .into_iter()
        .filter(|token| seen.insert(token.clone()))
        .take(64)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn folds_pdf_radicals_without_changing_code_literals() {
        let input = "⽇志⽅法：String s = \"Ａ①²\";\n下一行";
        let expected = "日志方法：String s = \"Ａ①²\";\n下一行";
        assert_eq!(normalize_radicals(input), expected);
        assert_eq!(normalize_radicals(expected), expected);
    }

    #[test]
    fn index_and_query_share_chinese_bigrams_and_compatibility_folding() {
        assert_eq!(
            query_tokens("异常⽇志异常日志 Ｊａｖａ"),
            vec!["异常", "常日", "日志", "志异", "Java"]
        );
        let projection = search_projection("处理异常⽇志信息 Java方法");
        for token in query_tokens("日志 方法 Ｊａｖａ 日") {
            assert!(projection.split_whitespace().any(|word| word == token));
        }
        assert!(query_tokens("，。！！！ AND\" OR")
            .iter()
            .all(|t| !t.contains('"')));
        assert!(query_tokens("，。！").is_empty());
        assert!(query_tokens(&"异常日志".repeat(1000)).len() <= 64);
    }
}
