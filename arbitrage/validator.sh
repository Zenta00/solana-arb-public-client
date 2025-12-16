#!/bin/bash
#export RUST_LOG=arbitrage=debug,info
#export RUST_BACKTRACE=full  # Add this line for verbose backtrace
export RUST_LOG=error
exec /home/thsmg/solana-arb-client/target/release/agave-validator \
    --identity /home/thsmg/solana-arb-client/validator-keypair.json \
    --known-validator 4H2owZLC5fzxjbkfLKpynSbpXBvXPzGk19V3Ucougcsr \
    --known-validator 6qwYjs5vCSEKaTMBbHinnW8fvdGj1r8cpzPoAV1EHKsw \
    --known-validator Fd7btgySsrjuo25CJCj7oE7VPMyezDhnx7pZkj2v69Nk \
    --known-validator 5pPRHniefFjkiaArbGX3Y8NUysJmQ9tMZg3FrFGwHzSm \
    --known-validator 6WgdYhhGE53WrZ7ywJA15hBVkw7CRbQ8yDBBTwmBtAHN \
    --known-validator 6TkKqq15wXjqEjNg9zqTKADwuVATR9dW3rkNnsYme1ea \
    --known-validator 7Np41oeYqPefeNQEHSv1UDhYrehxin3NStELsSKCT4K2 \
    --known-validator GdnSyH3YtwcxFvQrVVJMm1JhTS4QVX7MFsX56uJLUfiZ \
    --known-validator DE1bawNcRJB9rVm3buyMVfr8mBEoyyu73NBovf2oXJsJ \
    --known-validator CakcnaRDHka2gXyfbEd2d3xsvkJkqsLw2akB3zsN1D2S \
    --only-known-rpc \
    --full-rpc-api \
    --no-voting \
    --ledger /mnt/ledger \
    --accounts /mnt/accounts \
    --log /home/thsmg/solana-arb-client/solana-rpc.log \
    --rpc-port 8899 \
    --rpc-bind-address 0.0.0.0 \
    --private-rpc \
    --dynamic-port-range 8000-8020 \
    --entrypoint entrypoint.mainnet-beta.solana.com:8001 \
    --entrypoint entrypoint2.mainnet-beta.solana.com:8001 \
    --entrypoint entrypoint3.mainnet-beta.solana.com:8001 \
    --entrypoint entrypoint4.mainnet-beta.solana.com:8001 \
    --entrypoint entrypoint5.mainnet-beta.solana.com:8001 \
    --expected-genesis-hash 5eykt4UsFv8P8NJdTREpY1vzqKqZKvdpKuc147dw2N9d \
    --wal-recovery-mode skip_any_corrupted_record \
    --limit-ledger-size 50000000\
    --rpc-pubsub-notification-threads 0 \
    --rpc-pubsub-worker-threads 2 \
    --skip-poh-verify \
    --no-snapshot-fetch 
