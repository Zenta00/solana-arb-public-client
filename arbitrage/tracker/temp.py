import pandas as pd
import os
from typing import Dict, List, Tuple
from dataclasses import dataclass, field
from decimal import Decimal

SCALE_FEE_CONSTANT = 1_000_000_000
DEFAULT_FEE_BPS = 10_000_000
LAMPORTS_PER_SOL = 1_000_000_000

# Blacklisted addresses that will be ignored in analysis
BLACKLISTED_ADDRESSES = set()

# Read blacklisted addresses from file
with open("arbitrage/tracker/blacklist.txt", "r") as f:
    for line in f:
        address = line.strip()
        if address:  # Skip empty lines
            BLACKLISTED_ADDRESSES.add(address)

def get_amount_out_with_fee(
    amount_in: int,
    reserve_x: int,
    reserve_y: int,
    k: int,
    x_for_y: bool,
    fee: int = DEFAULT_FEE_BPS,
    inverse_fee: bool = False
) -> int:
    amount_in_with_fee = (
        amount_in * SCALE_FEE_CONSTANT // (SCALE_FEE_CONSTANT + fee)
        if inverse_fee
        else amount_in * (SCALE_FEE_CONSTANT - fee) // SCALE_FEE_CONSTANT
    )
    if x_for_y:
        return reserve_y - (k // (reserve_x + amount_in_with_fee))
    else:
        return reserve_x - (k // (reserve_y + amount_in_with_fee))

@dataclass
class TokenTrade:
    token_address: str
    total_sol_in: int = 0  # in lamports
    total_sol_out: int = 0  # in lamports
    total_tokens_bought: int = 0
    total_tokens_sold: int = 0
    total_tokens_held: int = 0
    is_completed: bool = False
    is_valid: bool = True  # Track if the trade is valid
    pnl: int = 0  # in lamports

    def add_trade(self, sol_amount: int, token_amount: int, is_buy: bool) -> None:
        if is_buy:
            self.total_sol_in += sol_amount
            self.total_tokens_bought += token_amount
            self.total_tokens_held += token_amount
        else:
            self.total_sol_out += sol_amount
            self.total_tokens_sold += token_amount
            self.total_tokens_held -= token_amount
            
            # Check if total tokens sold exceeds total tokens bought
            if self.total_tokens_sold > self.total_tokens_bought:
                self.is_valid = False
                self.pnl = 0  # Reset PnL to 0 if trade becomes invalid
                return
        
        # Only check for completion if the trade is valid
        if self.is_valid and self.total_tokens_sold > 0:
            difference = abs(self.total_tokens_bought - self.total_tokens_sold)
            tolerance = self.total_tokens_bought * 3/100  # 3% tolerance
            if difference <= tolerance:
                self.is_completed = True
                self.pnl = self.total_sol_out - self.total_sol_in if self.is_valid else 0

@dataclass
class CopyTraderPosition:
    token_address: str
    sol_invested: int = 0  # in lamports
    sol_sold: int = 0  # in lamports
    tokens_held: int = 0
    is_completed: bool = False
    pnl: int = 0  # in lamports

    def simulate_buy(self, whale_sol_amount: int, whale_token_amount: int, 
                    post_reserve_token: int, post_reserve_sol: int) -> None:
        # Copy trade with 10% of whale's amount
        sol_amount = (whale_sol_amount * 10) // 100  # This is equivalent to whale_sol_amount * 0.1 but avoids floating point
        self.sol_invested += sol_amount
        # Calculate amount out using the AMM formula
        k = post_reserve_token * post_reserve_sol
        tokens_out = get_amount_out_with_fee(
            sol_amount,
            post_reserve_token,
            post_reserve_sol,
            k,
            False  # y_to_x for buying tokens
        )
        self.tokens_held += tokens_out

    def simulate_sell(self, whale_sell_percentage: float, 
                     post_reserve_token: int, post_reserve_sol: int) -> None:
        if self.tokens_held == 0:
            return
            
        # Sell same percentage as whale
        tokens_to_sell = (self.tokens_held * whale_sell_percentage) // 100
        self.tokens_held -= tokens_to_sell
        
        # Calculate SOL received
        k = post_reserve_token * post_reserve_sol
        sol_out = get_amount_out_with_fee(
            tokens_to_sell,
            post_reserve_token,
            post_reserve_sol,
            k,
            True  # x_to_y for selling tokens
        )
        self.sol_sold += sol_out

@dataclass
class WhaleTrader:
    address: str
    token_trades: Dict[str, TokenTrade] = field(default_factory=dict)
    total_pnl: int = 0  # in lamports
    copy_trader_positions: Dict[str, CopyTraderPosition] = field(default_factory=dict)
    copy_trader_total_pnl: int = 0  # in lamports

    def add_trade(self, token_address: str, sol_amount: int, token_amount: int, 
                 is_buy: bool, post_reserve_token: int, post_reserve_sol: int) -> None:
        
        if token_address == "BY42VTMCqat9VXwsLLHjAV8EtGfn5iMCoS2dYTVzpump":
            print("found")
        # Original whale trade processing
        if token_address not in self.token_trades:
            self.token_trades[token_address] = TokenTrade(token_address)
            self.copy_trader_positions[token_address] = CopyTraderPosition(token_address)
        
        total_tokens = self.token_trades[token_address].total_tokens_held

        self.token_trades[token_address].add_trade(sol_amount, token_amount, is_buy)
        
        # Only proceed with copy trading if the trade is valid
        if not self.token_trades[token_address].is_valid:
            # Reset copy trader position for this token if trade becomes invalid
            self.copy_trader_positions[token_address] = CopyTraderPosition(token_address)
            return
            
        # Copy trader simulation
        if is_buy:
            self.copy_trader_positions[token_address].simulate_buy(
                sol_amount, token_amount, post_reserve_token, post_reserve_sol
            )
        else:
            # Calculate what percentage of total holdings the whale is selling
            
            if total_tokens > 0:
                sell_percentage = (token_amount * 100) // total_tokens
                if sell_percentage > 95:  # If whale sells more than 95%, consider it a full exit
                    sell_percentage = 100
                self.copy_trader_positions[token_address].simulate_sell(
                    sell_percentage, post_reserve_token, post_reserve_sol
                )
    

class WhaleAnalyzer:
    def __init__(self, charts_dir: str):
        self.charts_dir = charts_dir
        self.whales: Dict[str, WhaleTrader] = {}
        
    def process_csv_files(self) -> None:
        for filename in os.listdir(self.charts_dir):
            if filename.endswith('.csv'):
                token_address = filename[:-4]  # Remove .csv extension
                file_path = os.path.join(self.charts_dir, filename)
                
                df = pd.read_csv(file_path)
                for _, row in df.iterrows():
                    whale_address = row['whale_address']
                    # Skip blacklisted addresses
                    if whale_address in BLACKLISTED_ADDRESSES:
                        continue
                        
                    if whale_address not in self.whales:
                        self.whales[whale_address] = WhaleTrader(whale_address)
                    
                    self.whales[whale_address].add_trade(
                        token_address=token_address,
                        sol_amount=int(row['sol_amount']),  # Convert lamports to SOL
                        token_amount=int(row['token_amount']),
                        is_buy=bool(row['is_buy']),
                        post_reserve_token=int(row['post_token_reserve']),
                        post_reserve_sol=int(row['post_sol_reserve'])
                    )
    
    def get_results(self) -> Tuple[Dict[str, Tuple[int, int]], int, int]:
        whale_and_copy_pnls = {
            whale.address: (whale.total_pnl, whale.copy_trader_total_pnl) 
            for whale in self.whales.values()
        }
        total_whale_pnl = sum(pnl[0] for pnl in whale_and_copy_pnls.values())
        total_copy_pnl = sum(pnl[1] for pnl in whale_and_copy_pnls.values())
        return whale_and_copy_pnls, total_whale_pnl, total_copy_pnl

def analyze_wallet_trades(wallet_address: str, charts_dir: str) -> None:
    """
    Analyze and print all trades for a specific wallet address.
    
    Args:
        wallet_address: The wallet address to analyze
        charts_dir: Directory containing the CSV files with trade data
    """
    analyzer = WhaleAnalyzer(charts_dir)
    analyzer.process_csv_files()
    
    if wallet_address not in analyzer.whales:
        print(f"No trades found for wallet: {wallet_address}")
        return
        
    whale = analyzer.whales[wallet_address]
    
    print(f"\nDetailed Trade Analysis for Wallet: {wallet_address}")
    print("=" * 80)

    total_whale_pnl = 0
    total_copy_pnl = 0
    
    for token_address, token_trade in whale.token_trades.items():
        if not token_trade.is_completed or not token_trade.is_valid:
            continue
        
        pnl = token_trade.total_sol_out - token_trade.total_sol_in
        total_whale_pnl += pnl
        print(f"\nToken: {token_address}")
        print("-" * 50)
        print(f"Total SOL Invested: {token_trade.total_sol_in / LAMPORTS_PER_SOL:.4f} SOL")
        print(f"Total SOL Received: {token_trade.total_sol_out / LAMPORTS_PER_SOL:.4f} SOL")
        print(f"Total Tokens Bought: {token_trade.total_tokens_bought}")
        print(f"Total Tokens Sold: {token_trade.total_tokens_sold}")
        print(f"Position Status: {'Completed' if token_trade.is_completed else 'Open'}")
        print(f"PnL: {pnl / LAMPORTS_PER_SOL:.4f} SOL")
        
        # Copy trader analysis
        copy_position = whale.copy_trader_positions[token_address]
        copy_pnl = copy_position.sol_sold - copy_position.sol_invested
        total_copy_pnl += copy_pnl
        print("\nCopy Trader Following This Token:")
        print(f"SOL Invested: {copy_position.sol_invested / LAMPORTS_PER_SOL:.4f} SOL")
        print(f"Tokens Currently Held: {copy_position.tokens_held}")
        print(f"Copy Trader PnL: {copy_pnl / LAMPORTS_PER_SOL:.4f} SOL")
        print("-" * 50)
    
    print("\nSummary:")
    print(f"Total Whale PnL: {total_whale_pnl / LAMPORTS_PER_SOL:.4f} SOL")
    print(f"Total Copy Trader PnL: {total_copy_pnl / LAMPORTS_PER_SOL:.4f} SOL")
    print("=" * 80)

def main():
    charts_dir = "arbitrage/tracker/charts"
    
    # Original analysis
    analyzer = WhaleAnalyzer(charts_dir)
    analyzer.process_csv_files()
    
    whale_and_copy_pnls, total_whale_pnl, total_copy_pnl = analyzer.get_results()
    
    print("\nWhale and Copy Trader PnL Analysis")
    print("=" * 50)
    for whale_address, (whale_pnl, copy_pnl) in whale_and_copy_pnls.items():
        print(f"Whale {whale_address}: {whale_pnl / LAMPORTS_PER_SOL:.4f} SOL")
        print(f"Copy Trader following {whale_address}: {copy_pnl / LAMPORTS_PER_SOL:.4f} SOL")
        print("-" * 30)
    print("=" * 50)
    print(f"Total Whale PnL: {total_whale_pnl / LAMPORTS_PER_SOL:.4f} SOL")
    print(f"Total Copy Trader PnL: {total_copy_pnl / LAMPORTS_PER_SOL:.4f} SOL")
    
    # Example of analyzing a specific wallet
    analyze_wallet_trades("A9aTuBuxoVY547n6hUBCq9oZm36LTJX9Kvn4NZXffXvp", charts_dir)

if __name__ == "__main__":
    main() 