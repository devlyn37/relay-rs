use ethers::types::Chain;

fn get_prefix(chain: Chain) -> String {
    let prefix = match chain {
        Chain::Mainnet => "eth-mainnet",
        Chain::Goerli => "eth-goerli",
        Chain::Polygon => "polygon-mainnet",
        Chain::PolygonMumbai => "polygon-mumbai",
        Chain::Sepolia => "eth-sepolia",
        _ => panic!("chain {} not supported", chain),
    };

    prefix.to_owned()
}

pub fn get_ws(chain: Chain, key: &str) -> String {
    format!("wss://{}.g.alchemy.com/v2/{}", get_prefix(chain), key)
}
