tail -f solana-rpc.log // view logs 

setup performance mode
sudo apt install cpufrequtils

cat /sys/devices/system/cpu/cpu*/cpufreq/scaling_governor

for cpu in /sys/devices/system/cpu/cpu*/cpufreq/scaling_governor; do
    echo performance | sudo tee $cpu
done

sudo apt-get update \
&& sudo apt-get install python3-venv git -y \
&& git clone https://github.com/c29r3/solana-snapshot-finder.git \
&& cd solana-snapshot-finder \
&& python3 -m venv venv \
&& source ./venv/bin/activate \
&& pip3 install -r requirements.txt

python3 snapshot-finder.py --snapshot_path /mnt/ledger -t 10 --wildcard_version 2.1 --with_private_rpc --min_download_speed 35

deactivate
nohup cargo run --release --package arbitrage --bin arbitrage > output.log 2>&1 &
nohup /home/thsmg/solana-arb-client/target/release/arbitrage > output.log 2>&1 & //no recompile
nohup /home/thsmg/solana-arb-client/target/alternate/release/arbitrage > output-dev.log 2>&1 &
nohup ./monitor.sh > monitor.log 2>&1 &
nohup ./monitor-dev.sh > monitor-dev.log 2>&1 &
cargo build --release --package arbitrage --bin arbitrage --target-dir=target/alternate  //dev build
2574730
sudo swapoff -a
swapon --show

RUST_LOG=info cargo run --release --bin jito-shredstream-proxy -- shredstream --block-engine-url https://amsterdam.mainnet.block-engine.jito.wtf --auth-keypair my_keypair.json --desired-regions amsterdam,frankfurt,london,ny,tokyo --dest-ip-ports 127.0.0.1:8001

./target/release/solana catchup --our-localhost --url https://api.mainnet-beta.solana.com