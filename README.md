# Relay

An Ethereum Transaction Relay that:
- Manages the nonce of a single address
- Makes sure transactions get included

## Routes

`POST /transaction`

`GET /transaction/:id`

## Database Setup

This Project uses `MySQL` and `sqlx` right now.

First, make sure to set `DATABASE_URL` in `.env`

next, install sqlx-cli
```
cargo install sqlx-cli
```

run migrations
```
sqlx migrate run
```

You're good to go, everything should compile at this point!

## Making Schema Changes

Create a new migration file

```
sqlx migrate add <name>
```

Then add SQL to the newly created file.
## TODO
- Testing
- Init/Recovery Sequence
- Multiple Addresses
- Multi Chain
	- Make sure gas estimation works reasonably across 1559 chains (looking at you polygon)
	- All EVM Chains