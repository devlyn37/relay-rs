# Relay

An Ethereum Transaction Relay that:
- Manages the nonce of a single address
- Makes sure transactions get included

## Routes

`POST /transaction`

`GET /transaction/:id`

## TODO
- Init/Recovery Sequence
- Multiple Addresses
- Multi Chain
	- All EIP-1559 Chains
	- All EVM Chains