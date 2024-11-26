mod chunker;
mod dedup_testing;
mod proxy_testing;
mod packet_parser;
mod io_utils;

fn main() {
    // dedup_testing::dedup_test();
    proxy_testing::proxy_test();
}