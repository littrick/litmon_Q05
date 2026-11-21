//! 查询一次 Q0.5 设备在线状态。
//!
//! 运行：cargo run -p q05-session --example presence_probe

fn main() {
    match q05_session::query_presence() {
        Ok(presence) => println!("presence: {presence:?}"),
        Err(err) => eprintln!("presence error: {err}"),
    }
}
