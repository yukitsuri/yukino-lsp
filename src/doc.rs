use crate::hover::get_keyword_doc;

pub fn run_doc(symbol: &str, json_mode: bool) {
    let doc_str = get_keyword_doc(symbol).unwrap_or("Symbol documentation not found.");

    if json_mode {
        let json = serde_json::json!({
            "symbol": symbol,
            "doc": doc_str,
        });
        println!("{json}");
    } else {
        println!("{doc_str}");
    }
}
