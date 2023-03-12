use ethers::types::Chain;

pub fn get_rpc(chain: Chain, key: &str) -> String {
    let prefix = match chain {
        Chain::Mainnet => "eth-mainnet",
        Chain::Goerli => "eth-goerli",
        Chain::Polygon => "polygon-mainnet",
        Chain::PolygonMumbai => "polygon-mumbai",
        Chain::Sepolia => "eth-sepolia",
        _ => panic!("chain {} not supported", chain),
    };

    format!("https://{}.g.alchemy.com/v2/{}", prefix, key)
}
