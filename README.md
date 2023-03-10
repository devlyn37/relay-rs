# Relay

An Ethereum Transaction Relay that:
- Manages the nonce of a single address
- Makes sure transactions get included

## Routes

`POST /transaction`

`GET /transaction/:id`

## TODO
- improve API Surface
    - 404s
    - auth
- Init/Recovery Sequence
- Multiple Addresses 
- Multi Chain