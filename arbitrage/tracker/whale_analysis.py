import pandas as pd
from dataclasses import dataclass
from typing import List, Set, Dict
from pathlib import Path
import numpy as np
import random
SCALE_FEE_CONSTANT = 1_000_000_000
DEFAULT_FEE_BPS = 10_000_000
LAMPORTS_PER_SOL = 1_000_000_000

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
class Swap:
    timestamp: int
    slot: int
    sol_amount: int
    token_amount: int
    avg_mc: int
    post_sol_reserve: int
    post_token_reserve: int
    is_buy: bool
    whale_address: str

class Token:
    def __init__(self, address: str, swaps: List[Swap]):
        self.address = address
        self.swaps = swaps

    def __eq__(self, other: 'Token') -> bool:
        if not isinstance(other, Token):
            return False
            
        if self.address != other.address:
            return False
            
        if len(self.swaps) != len(other.swaps):
            return False
            
        for swap1, swap2 in zip(self.swaps, other.swaps):
            if (swap1.timestamp != swap2.timestamp or
                swap1.slot != swap2.slot or
                swap1.sol_amount != swap2.sol_amount or
                swap1.token_amount != swap2.token_amount or
                swap1.avg_mc != swap2.avg_mc or
                swap1.post_sol_reserve != swap2.post_sol_reserve or
                swap1.post_token_reserve != swap2.post_token_reserve or
                swap1.is_buy != swap2.is_buy or
                swap1.whale_address != swap2.whale_address):
                return False
                
        return True

def load_whales() -> Set[str]:
    # Load whales and blacklist
    whales_file = Path("/home/thsmg/solana-arb-client/arbitrage/tracker/whales.txt")
    
    whales: Set[str] = set()
    
    if whales_file.exists():
        with open(whales_file, "r") as f:
            whales = set(line.strip() for line in f.readlines())
            
    return whales

def load_blacklist() -> Set[str]:
    # Load whales and blacklist
    blacklist_file = Path("/home/thsmg/solana-arb-client/arbitrage/tracker/blacklist.txt")
    
    blacklist: Set[str] = set()
    
    if blacklist_file.exists():
        with open(blacklist_file, "r") as f:
            blacklist = set(line.strip() for line in f.readlines())
    
    return blacklist

def load_token(token_address: str) -> Token:
    
    # Load and process CSV data
    csv_path = Path(f"/home/thsmg/solana-arb-client/arbitrage/tracker/charts/{token_address}.csv")
    if not csv_path.exists():
        raise FileNotFoundError(f"CSV file not found for token {token_address}")
        
    df = pd.read_csv(csv_path)
    
    # Convert DataFrame rows to Swap objects
    swaps = []
    for _, row in df.iterrows():
            
        swap = Swap(
            timestamp=int(row['timestamp']),
            slot=int(row['slot']),
            sol_amount=int(row['sol_amount']),
            token_amount=int(row['token_amount']),
            avg_mc=int(row['avg_mc']),
            post_sol_reserve=int(row['post_sol_reserve']),
            post_token_reserve=int(row['post_token_reserve']),
            is_buy=bool(row['is_buy']),
            whale_address=str(row['whale_address'])
        )
        swaps.append(swap)
    
    return Token(token_address, swaps)

def load_token_fast(token_address: str) -> Token:
    csv_path = Path(f"/home/thsmg/solana-arb-client/arbitrage/tracker/charts/{token_address}.csv")
    if not csv_path.exists():
        raise FileNotFoundError(f"CSV file not found for token {token_address}")
    
    # Use basic csv reader but maintain exact order
    swaps = []
    with open(csv_path, 'r') as f:
        # Skip header
        header = next(f).strip().split(',')
        # Create index mapping for each column
        col_idx = {col: idx for idx, col in enumerate(header)}
        
        # Read lines directly to maintain exact order
        for line in f:
            row = line.strip().split(',')
            swap = Swap(
                timestamp=int(row[col_idx['timestamp']]),
                slot=int(row[col_idx['slot']]),
                sol_amount=int(row[col_idx['sol_amount']]),
                token_amount=int(row[col_idx['token_amount']]),
                avg_mc=int(row[col_idx['avg_mc']]),
                post_sol_reserve=int(row[col_idx['post_sol_reserve']]),
                post_token_reserve=int(row[col_idx['post_token_reserve']]),
                is_buy=str(row[col_idx['is_buy']]).lower() in ['true', '1'],
                whale_address=str(row[col_idx['whale_address']])
            )
            swaps.append(swap)
    
    return Token(token_address, swaps)
def load_all_tokens(limit: int = 100) -> List[Token]:
    # Get the charts directory path
    charts_dir = Path("/home/thsmg/solana-arb-client/arbitrage/tracker/charts")
    
    # Initialize list to store all tokens
    tokens = []
    
    # Check if directory exists
    if not charts_dir.exists():
        raise FileNotFoundError("Charts directory not found")
    
    counter = 0
    # Iterate through all CSV files in the directory
    for csv_file in charts_dir.glob("*.csv"):
        try:
            if counter >= limit:
                break
            # Extract token address from filename (removing .csv extension)
            token_address = csv_file.stem
            
            # Load the token using existing load_token function
            token = load_token_fast(token_address)
            tokens.append(token)
            counter += 1
        except Exception as e:
            print(f"Error loading token from {csv_file}: {str(e)}")
            counter += 1
            continue
    tokens.sort(key=lambda token: token.swaps[0].timestamp if token.swaps else 0)

    return tokens

@dataclass
class TradePosition:
    total_sol_in: int = 0
    total_sol_out: int = 0
    total_token_bought: int = 0
    total_token_sold: int = 0
    synced: bool = True
    num_buys: int = 0
    num_sells: int = 0
    
    @property
    def total_token_held(self) -> int:
        if self.total_token_bought < self.total_token_sold:
            self.synced = False
            return 0
        return self.total_token_bought - self.total_token_sold
    
    def buy(self, sol_amount: int, post_sol_reserve: int, post_token_reserve: int) -> int:
        # Calculate k after the swap we're copying
        k = post_sol_reserve * post_token_reserve
        
        # Calculate how many tokens we get for our SOL
        tokens_received = get_amount_out_with_fee(
            amount_in=sol_amount,
            reserve_x=post_token_reserve,
            reserve_y=post_sol_reserve,
            k=k,
            x_for_y=False
        )
        ##tokens_received = tokens_received * 99 // 100
        
        self.total_sol_in += sol_amount
        self.total_token_bought += tokens_received
        self.num_buys += 1
        return tokens_received
    
    def sell(self, token_amount: int, post_sol_reserve: int, post_token_reserve: int) -> int:
        # Calculate k after the swap we're copying
        k = post_sol_reserve * post_token_reserve
        
        # Calculate how much SOL we get for our tokens
        sol_received = get_amount_out_with_fee(
            amount_in=token_amount,
            reserve_x=post_token_reserve,
            reserve_y=post_sol_reserve,
            k=k,
            x_for_y=True
        )
        
        self.total_sol_out += sol_received
        self.total_token_sold += token_amount
        self.num_sells += 1
        return sol_received

    def print(self, token_address: str = None, address: str = None):
        """
        Print the trade position details
        
        Args:
            token_address (str, optional): Token address to include in PNL output
            address (str, optional): Address to show as position owner
        """
        print("-" * 50)
        if address:
            print(f"{address}: ")
        print(f"Total sol in: {self.total_sol_in / 1_000_000_000.0}")
        print(f"Total sol out: {self.total_sol_out / 1_000_000_000.0}")
        print(f"Total token bought: {self.total_token_bought}")
        print(f"Total token sold: {self.total_token_sold}")
        print(f"Total token held: {self.total_token_held}")
        print(f"Synced: {self.synced}")
        print(f"Num buys: {self.num_buys}")
        print(f"Num sells: {self.num_sells}")
        pnl = str((self.total_sol_out - self.total_sol_in) / 1_000_000_000.0) if self.synced else "Uncompleted"
        print(f"Pnl: {pnl}" + (f" in {token_address}" if token_address else ""))
        print("-" * 50)

@dataclass
class CopyTradePosition(TradePosition):
    def __init__(self):
        super().__init__()
        self.related_whale_positions: Dict[str, TradePosition] = {}
    
    def get_whale_position(self, whale_address: str) -> TradePosition:
        """Get or create a position tracking for a specific whale"""
        if whale_address not in self.related_whale_positions:
            self.related_whale_positions[whale_address] = TradePosition()
        return self.related_whale_positions[whale_address]
    
    def buy(self, sol_amount: int, post_sol_reserve: int, post_token_reserve: int, whale_address: str) -> int:
        """Extended buy that also tracks per-whale position"""
        tokens_received = super().buy(sol_amount, post_sol_reserve, post_token_reserve)
        
        # Track this trade for the specific whale
        whale_position = self.get_whale_position(whale_address)
        whale_position.total_sol_in += sol_amount
        whale_position.total_token_bought += tokens_received
        whale_position.num_buys += 1
        return tokens_received
    
    def sell(self, token_amount: int, post_sol_reserve: int, post_token_reserve: int, whale_address: str) -> int:
        """Extended sell that also tracks per-whale position"""
        sol_received = super().sell(token_amount, post_sol_reserve, post_token_reserve)
        
        # Track this trade for the specific whale
        whale_position = self.get_whale_position(whale_address)
        whale_position.total_sol_out += sol_received
        whale_position.total_token_sold += token_amount
        whale_position.num_sells += 1
        return sol_received

class CopyTrader:
    def __init__(self, buy_pct: int = 10, strategy: str = "first_lead"):
        """
        Initialize CopyTrader
        
        Args:
            buy_pct (float): Percentage of the swap amount to copy (0.0 to 1.0)
        """
        self.positions: Dict[str, CopyTradePosition] = {}
        self.buy_pct = buy_pct
        self.strategy = strategy
        self.scores: Dict[str, int] = {}  # whale_address -> score mapping

    def update_whale_scores(self, whales_seen: Set[str], pnl: int):
        for whale_address in whales_seen:
            if whale_address not in self.scores:
                self.scores[whale_address] = 0
            self.scores[whale_address] += pnl
    
    def update_whale_score(self, whale_address: str, pnl: int):
        """Update the score for a whale by adding/subtracting PNL"""
        if whale_address not in self.scores:
            self.scores[whale_address] = 0
        self.scores[whale_address] += pnl

    def print_worst_whale_scores(self, limit: int = 10):
        """
        Print worst whale scores in descending order within the specified range
        
        Args:
            limit (int): Number of whales to print
        """
        # Sort whales by score in descending order
        sorted_whales = sorted(self.scores.items(), key=lambda x: x[1], reverse=True)
        

        
        # Ensure indices are within bounds
        len_sorted_whales = len(sorted_whales)
        start_index = len_sorted_whales - limit
        end_index = len_sorted_whales

        
        print("\n=== Top Whale Scores ===")
        print(f"Showing whales {start_index+1} to {end_index}")
        print("-" * 50)
        
        for i, (whale_address, score) in enumerate(sorted_whales[start_index:end_index], start=start_index + 1):
            print(f"{i}. Whale: {whale_address}")
            print(f"   Score: {score:.4f} SOL")
            print("-" * 50)

    def print_top_whale_scores(self, start_index: int = 0, end_index: int = None):
        """
        Print whale scores in descending order within the specified range
        
        Args:
            start_index (int): Starting index (0-based)
            end_index (int): Ending index (exclusive). If None, prints until the end
        """
        # Sort whales by score in descending order
        sorted_whales = sorted(self.scores.items(), key=lambda x: x[1], reverse=True)
        
        # Adjust end_index if not specified
        if end_index is None:
            end_index = len(sorted_whales)
        
        # Ensure indices are within bounds
        start_index = max(0, min(start_index, len(sorted_whales)))
        end_index = max(0, min(end_index, len(sorted_whales)))
        
        print("\n=== Top Whale Scores ===")
        print(f"Showing whales {start_index+1} to {end_index}")
        print("-" * 50)
        
        for i, (whale_address, score) in enumerate(sorted_whales[start_index:end_index], start=start_index + 1):
            print(f"{i}. Whale: {whale_address}")
            print(f"   Score: {score:.4f} SOL")
            print("-" * 50)
    
    def get_position(self, token_address: str) -> CopyTradePosition:
        """Get or create a position for a token"""
        if token_address not in self.positions:
            self.positions[token_address] = CopyTradePosition()
        return self.positions[token_address]
    
    def make_decision(self, token_address: str, swap: Swap, total_token_held_by_whale: int) -> tuple[int, int]:
        position = self.get_position(token_address)
        
        if swap.is_buy:
            ###our_sol_amount = swap.sol_amount * self.buy_pct // 100
            
            if swap.sol_amount > 200_000_000:
                our_sol_amount = 50_000_000
            else:
                return 0, 0

            if swap.token_amount > 80_000_000_000_000:
                return 0, 0
                
            tokens_received = position.buy(
                our_sol_amount,
                swap.post_sol_reserve,
                swap.post_token_reserve,
                swap.whale_address
            )
            return our_sol_amount, tokens_received
        else:
            whale_sell_pct = swap.token_amount * 100 // total_token_held_by_whale
            if whale_sell_pct > 98:
                whale_sell_pct = 100
            our_token_amount = 0
            if self.strategy == "mimic":
                our_token_amount = position.get_whale_position(swap.whale_address).total_token_held * whale_sell_pct // 100
            elif self.strategy == "first_lead":
                our_token_amount = position.total_token_held * whale_sell_pct // 100
            
            if our_token_amount > 0:
                sol_received = position.sell(
                    our_token_amount,
                    swap.post_sol_reserve,
                    swap.post_token_reserve,
                    swap.whale_address
                )
                return sol_received, our_token_amount
            
        return 0, 0


class Whale:
    def __init__(self, address: str):
        self.address = address
        self.positions = dict()
        
    def update_position(self, swap: Swap, token_address: str) -> bool:
        

        position = self.positions[token_address]
        if swap.is_buy:
            position.total_sol_in += swap.sol_amount
            position.total_token_bought += swap.token_amount
            position.num_buys += 1
        else:
            if position.total_token_held == 0:
                return False
            if position.total_token_sold + swap.token_amount > position.total_token_bought:
                return False
            position.total_sol_out += swap.sol_amount
            position.total_token_sold += swap.token_amount
            position.num_sells += 1
        return True

def simulate_token(token: Token, whales_tracked: Set[str], copy_trader: CopyTrader) -> tuple[dict[str, Whale], int, bool]:
    current_whales: dict[str, Whale] = {}
    at_least_one_copy_trade = False
    token_address = token.address
    whales_seen = set()
    for swap in token.swaps:
        # If this is a new whale address we haven't seen
        if swap.whale_address in whales_tracked:
            if swap.whale_address not in current_whales:
                current_whales[swap.whale_address] = Whale(swap.whale_address)
            # Update the whale's position
            whale = current_whales[swap.whale_address]
            
            if token_address not in whale.positions:
                whale.positions[token_address] = TradePosition()
            whale_position = whale.positions[token_address].total_token_held

            if whale.update_position(swap, token_address):
                sol_received, token_amount = copy_trader.make_decision(token_address, swap, whale_position)
                
                if sol_received > 0 or token_amount > 0:
                    whales_seen.add(swap.whale_address)
                    at_least_one_copy_trade = True
                    
    unrealized_pnl = 0
    if at_least_one_copy_trade:
        token_held = copy_trader.positions[token.address].total_token_held
        if token_held > 0:
            last_swap = token.swaps[-1]
            last_swap_k = last_swap.post_sol_reserve * last_swap.post_token_reserve
            unrealized_pnl = get_amount_out_with_fee(
                amount_in=token_held,
                reserve_x=last_swap.post_token_reserve,
                reserve_y=last_swap.post_sol_reserve,
                k=last_swap_k,
                x_for_y=True
            )
            copy_position = copy_trader.positions[token.address]
            copy_trader.update_whale_scores(whales_seen, to_sol(copy_position.total_sol_out - copy_position.total_sol_in + unrealized_pnl))
    return (current_whales, unrealized_pnl, at_least_one_copy_trade)
def to_sol(amount: int) -> float:
    """Convert lamports to SOL"""
    return round(amount / 1_000_000_000.0, 4)

def load_tokens_and_whales(limit: int = 1000000000) -> tuple[List[Token], Set[str]]:
    tokens = load_all_tokens(limit=limit)
    whales_tracked = load_whales()
    blacklist = load_blacklist()
    whales_tracked = whales_tracked - blacklist
    return tokens, whales_tracked

def analyze_all_tokens(strategy: str = "mimic", tokens: List[Token] = None, whales_tracked: Set[str] = None, limit: int = 10000000):
    copy_trader = CopyTrader(strategy=strategy)
    total_whales_pnl = 0
    total_copy_trader_pnl = 0
    ordered_roi = []
    total_tx_number = 0
    
    for token in tokens:
        (whales, unrealized_pnl, at_least_one_copy_trade) = simulate_token(token, whales_tracked, copy_trader)
        if at_least_one_copy_trade:
            copy_position = copy_trader.positions[token.address]
            copy_total_sol_in = copy_position.total_sol_in
            copy_total_sol_out = copy_position.total_sol_out
            total_tx_number += copy_position.num_buys + copy_position.num_sells
            copy_pnl = copy_position.total_sol_out - copy_position.total_sol_in + unrealized_pnl
            total_whales_pnl += sum(whale.positions[token.address].total_sol_out - whale.positions[token.address].total_sol_in for whale in whales.values())
            total_copy_trader_pnl += copy_pnl
            ordered_roi.append((to_sol(copy_pnl) , round(copy_pnl / copy_total_sol_in, 3), token.address, to_sol(copy_total_sol_in), to_sol(copy_total_sol_out + unrealized_pnl)))

    print(f"Total whales PNL: {total_whales_pnl / 1_000_000_000.0}")
    print(f"Total copy trader PNL: {total_copy_trader_pnl / 1_000_000_000.0}")
    print(f"Total tx number: {total_tx_number}")
    ###ordered_roi.sort(key=lambda x: x[1], reverse=True)
    return (ordered_roi, copy_trader)


def analyze_token(token_address: str, whales: Set[str], strategy: str = "mimic"):
    token = load_token(token_address)
    whales_tracked = load_whales()
    copy_trader = CopyTrader(strategy=strategy)
    (whales, unrealized_pnl, at_least_one_copy_trade) = simulate_token(token, whales_tracked, copy_trader)
    
    if at_least_one_copy_trade:
        for whale in whales.values():
            print(f"\nWhale position : {whale.address}\n")
            whale.positions[token_address].print(token_address)

            if strategy == "mimic":
                print(f"\nCopy position :\n")
                copy_trader.positions[token_address].related_whale_positions[whale.address].print(token_address)

        print(f"Copy trader :\n")
        copy_trader.positions[token_address].print(token_address)
        print(f"Unrealized PNL: {unrealized_pnl / 1_000_000_000.0}")
    else:
        print(f"No copy trade for {token_address}")


tokens, whales_tracked = load_tokens_and_whales(limit=10000000)

(ordered_roi, copy_trader) = analyze_all_tokens(strategy="first_lead", tokens=tokens, whales_tracked=whales_tracked)
print(f"Total tokens: {len(tokens)}")


roi_values = np.array([roi[1] for roi in ordered_roi])
pnl_values = np.array([roi[0] for roi in ordered_roi])

avg_roi = np.mean(roi_values)
std_roi = np.std(roi_values)
median_roi = np.median(roi_values)

###for roi in ordered_roi[:-100]:
    ###print(roi)

print(f"Avg ROI: {avg_roi}")
print(f"Std ROI: {std_roi}")
print(f"Median ROI: {median_roi}")

avg_pnl = np.mean(pnl_values)
std_pnl = np.std(pnl_values)
median_pnl = np.median(pnl_values)

min_pnl = 0
max_pnl = 0
current_pnl = 0
for pnl in pnl_values:
    current_pnl += pnl
    if current_pnl < min_pnl:
        min_pnl = current_pnl
    if current_pnl > max_pnl:
        max_pnl = current_pnl

print(f"Avg PNL: {avg_pnl}")
print(f"Std PNL: {std_pnl}")
print(f"Median PNL: {median_pnl}")
print(f"Min Raw PNL: {np.min(pnl_values)}")
print(f"Max Raw PNL: {np.max(pnl_values)}")
print(f"Min total PNL: {min_pnl}")
print(f"Max total PNL: {max_pnl}")

#copy_trader.print_top_whale_scores(0, 25)
#copy_trader.print_worst_whale_scores(25)
# Sum PNL in chunks of 100
chunk_size = 500
for i in range(0, len(pnl_values), chunk_size):
    chunk = pnl_values[i:i + chunk_size]
    chunk_pnl = np.sum(chunk)
    print(f"Chunk {i//chunk_size + 1} (trades {i+1}-{min(i+chunk_size, len(pnl_values))}): Total PNL = {chunk_pnl:.4f}")




