pub fn build_ws_url(relay_url: &str, room_id: &str, role: &str) -> String {
    let (without_fragment, fragment) = relay_url
        .split_once('#')
        .map_or((relay_url, ""), |(url, fragment)| (url, fragment));
    let (path_part, query) = without_fragment
        .split_once('?')
        .map_or((without_fragment, ""), |(path, query)| (path, query));
    let mut path = path_part.trim_end_matches('/').to_string();
    if !path.ends_with("/ws") {
        path.push_str("/ws");
    }

    let mut url = if query.is_empty() {
        format!("{path}?room_id={room_id}&role={role}")
    } else {
        format!("{path}?{query}&room_id={room_id}&role={role}")
    };
    if !fragment.is_empty() {
        url.push('#');
        url.push_str(fragment);
    }
    url
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn appends_ws_path_when_relay_url_is_host_only() {
        assert_eq!(
            build_ws_url("ws://127.0.0.1:8787", "room-1", "app"),
            "ws://127.0.0.1:8787/ws?room_id=room-1&role=app"
        );
    }

    #[test]
    fn preserves_existing_ws_path() {
        assert_eq!(
            build_ws_url("ws://127.0.0.1:8787/ws", "room-1", "cli"),
            "ws://127.0.0.1:8787/ws?room_id=room-1&role=cli"
        );
    }

    #[test]
    fn preserves_existing_query_parameters() {
        assert_eq!(
            build_ws_url("wss://relay.example.com/ws?token=abc", "room-1", "cli"),
            "wss://relay.example.com/ws?token=abc&room_id=room-1&role=cli"
        );
    }

    #[test]
    fn appends_ws_path_before_existing_query_parameters() {
        assert_eq!(
            build_ws_url("wss://relay.example.com?token=abc", "room-1", "app"),
            "wss://relay.example.com/ws?token=abc&room_id=room-1&role=app"
        );
    }
}
