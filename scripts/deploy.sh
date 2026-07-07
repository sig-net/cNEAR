#!/usr/bin/env bash
set -e

# Colors for output
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
NC='\033[0m' # No Color

# Store original arg count for interactive mode detection
ORIGINAL_ARGC=$#

# Parse command line arguments
NETWORK=""
SIGNER_ID=""
DRY_RUN=false
TEST_MODE=false

while [[ $# -gt 0 ]]; do
    case $1 in
        testnet|mainnet)
            NETWORK="$1"
            shift
            ;;
        --dry-run)
            DRY_RUN=true
            shift
            ;;
        --test-mode)
            TEST_MODE=true
            shift
            ;;
        --help|-h)
            echo "Usage: $0 [testnet|mainnet] [signer_id] [--dry-run]"
            echo ""
            echo "Options:"
            echo "  testnet|mainnet    Target network (default: testnet)"
            echo "  signer_id          Account ID to use for deployment"
            echo "  --dry-run          Show commands without executing"
            echo "  --test-mode        Quick deployment to testnet (only prompt for signer)"
            echo ""
            echo "Examples:"
            echo "  $0                              # Interactive mode"
            echo "  $0 testnet                      # Interactive signer selection"
            echo "  $0 testnet alice.testnet        # Full CLI mode"
            echo "  $0 mainnet bob.near --dry-run   # Dry run"
            echo ""
            echo "Via justfile:"
            echo "  just deploy test                # Quick testnet deployment"
            exit 0
            ;;
        *)
            if [[ -z "$SIGNER_ID" ]]; then
                SIGNER_ID="$1"
            else
                echo -e "${RED}Error: Unknown argument '$1'${NC}"
                exit 1
            fi
            shift
            ;;
    esac
done

# Interactive network selection if not provided (skip in test mode)
if [[ -z "$NETWORK" && "$TEST_MODE" == "false" ]]; then
    echo -e "${BLUE}Select network:${NC}"
    echo "  1) testnet (default)"
    echo "  2) mainnet"
    read -p "Enter choice [1]: " network_choice
    network_choice=${network_choice:-1}
    
    case $network_choice in
        1) NETWORK="testnet" ;;
        2) NETWORK="mainnet" ;;
        *) echo -e "${RED}Invalid choice. Using testnet.${NC}"; NETWORK="testnet" ;;
    esac
elif [[ -z "$NETWORK" ]]; then
    # Test mode defaults to testnet
    NETWORK="testnet"
fi

echo -e "${GREEN}Network: $NETWORK${NC}"

# Determine credentials directory
if [[ -n "$NEAR_CREDENTIALS" ]]; then
    CREDS_DIR="$NEAR_CREDENTIALS/$NETWORK"
elif [[ -d "$HOME/.near-credentials/$NETWORK" ]]; then
    CREDS_DIR="$HOME/.near-credentials/$NETWORK"
else
    echo -e "${RED}Error: NEAR credentials directory not found${NC}"
    echo "Expected: ~/.near-credentials/$NETWORK or \$NEAR_CREDENTIALS/$NETWORK"
    exit 1
fi

if [[ ! -d "$CREDS_DIR" ]]; then
    echo -e "${RED}Error: Credentials directory does not exist: $CREDS_DIR${NC}"
    exit 1
fi

# Interactive signer selection if not provided
if [[ -z "$SIGNER_ID" ]]; then
    echo -e "\n${BLUE}Available accounts in $CREDS_DIR:${NC}"
    
    # List all .json files and extract account IDs
    ACCOUNTS=()
    i=1
    while IFS= read -r file; do
        account_id=$(basename "$file" .json)
        ACCOUNTS+=("$account_id")
        echo "  $i) $account_id"
        ((i++))
    done < <(find "$CREDS_DIR" -maxdepth 1 -name "*.json" -type f | sort)
    
    if [[ ${#ACCOUNTS[@]} -eq 0 ]]; then
        echo -e "${RED}Error: No account credentials found in $CREDS_DIR${NC}"
        exit 1
    fi
    
    if [[ "$TEST_MODE" == "true" ]]; then
        read -p "Select signer account for test deployment [1]: " account_choice
    else
        read -p "Select account [1]: " account_choice
    fi
    account_choice=${account_choice:-1}
    
    if [[ $account_choice -lt 1 || $account_choice -gt ${#ACCOUNTS[@]} ]]; then
        echo -e "${RED}Invalid choice${NC}"
        exit 1
    fi
    
    SIGNER_ID="${ACCOUNTS[$((account_choice-1))]}"
fi

echo -e "${GREEN}Signer: $SIGNER_ID${NC}"

# Verify signer credentials exist
SIGNER_KEY_FILE="$CREDS_DIR/${SIGNER_ID}.json"
if [[ ! -f "$SIGNER_KEY_FILE" ]]; then
    if [[ "$DRY_RUN" == "false" ]]; then
        echo -e "${RED}Error: Credentials file not found: $SIGNER_KEY_FILE${NC}"
        exit 1
    else
        echo -e "${YELLOW}Warning: Credentials file not found: $SIGNER_KEY_FILE (continuing in dry-run)${NC}"
    fi
else
    echo -e "${GREEN}✓ Credentials found${NC}"
fi

# Check if wasms exist (warn but continue in dry-run)
TOKEN_WASM="target/near/fungible_token.wasm"
CONTROLLER_WASM="target/near/aurora-controller-factory.wasm"

if [[ ! -f "$TOKEN_WASM" ]]; then
    if [[ "$DRY_RUN" == "false" ]]; then
        echo -e "${RED}Error: Token wasm not found at $TOKEN_WASM${NC}"
        echo "Run: just build-token"
        exit 1
    else
        echo -e "${YELLOW}Warning: Token wasm not found at $TOKEN_WASM (continuing in dry-run)${NC}"
    fi
fi

if [[ ! -f "$CONTROLLER_WASM" ]]; then
    if [[ "$DRY_RUN" == "false" ]]; then
        echo -e "${RED}Error: Controller wasm not found at $CONTROLLER_WASM${NC}"
        echo "Run: just build-controller"
        exit 1
    else
        echo -e "${YELLOW}Warning: Controller wasm not found at $CONTROLLER_WASM (continuing in dry-run)${NC}"
    fi
fi

if [[ -f "$TOKEN_WASM" && -f "$CONTROLLER_WASM" ]]; then
    echo -e "${GREEN}✓ Wasm files found${NC}"
fi

# Deployment configuration prompts
# - Fully interactive mode: prompt for everything
# - Test mode: use defaults, skip prompts
# - CLI mode: use defaults
if [[ $ORIGINAL_ARGC -eq 0 && "$TEST_MODE" == "false" ]]; then
    # Fully interactive - prompt for all config
    echo -e "\n${BLUE}Deployment Configuration:${NC}"
    read -p "Controller account ID [controller.$SIGNER_ID]: " CONTROLLER_ID
    CONTROLLER_ID=${CONTROLLER_ID:-"controller.$SIGNER_ID"}
    
    read -p "Token account ID [token.$SIGNER_ID]: " TOKEN_ID
    TOKEN_ID=${TOKEN_ID:-"token.$SIGNER_ID"}
    
    read -p "Token name [Controlled NEAR]: " TOKEN_NAME
    TOKEN_NAME=${TOKEN_NAME:-"Controlled NEAR"}
    
    read -p "Token symbol [cNEAR]: " TOKEN_SYMBOL
    TOKEN_SYMBOL=${TOKEN_SYMBOL:-"cNEAR"}
    
    read -p "Token decimals [24]: " TOKEN_DECIMALS
    TOKEN_DECIMALS=${TOKEN_DECIMALS:-24}
    
    read -p "Total supply [1000000000000000]: " TOTAL_SUPPLY
    TOTAL_SUPPLY=${TOTAL_SUPPLY:-"1000000000000000"}
    
    read -p "Initial balance for new accounts in NEAR [10]: " INITIAL_BALANCE
    INITIAL_BALANCE=${INITIAL_BALANCE:-10}
else
    # Use defaults for CLI mode and test mode
    CONTROLLER_ID="controller.$SIGNER_ID"
    TOKEN_ID="token.$SIGNER_ID"
    TOKEN_NAME="Controlled NEAR"
    TOKEN_SYMBOL="cNEAR"
    TOKEN_DECIMALS=24
    TOTAL_SUPPLY="1000000000000000"
    INITIAL_BALANCE=10
    
    if [[ "$TEST_MODE" == "true" ]]; then
        echo -e "${BLUE}Test mode: Using default configuration${NC}"
    fi
fi

# Show deployment summary
echo -e "\n${BLUE}=== Deployment Summary ===${NC}"
echo "Network:           $NETWORK"
echo "Signer:            $SIGNER_ID"
echo "Controller:        $CONTROLLER_ID"
echo "Token:             $TOKEN_ID"
echo "Token Name:        $TOKEN_NAME"
echo "Token Symbol:      $TOKEN_SYMBOL"
echo "Token Decimals:    $TOKEN_DECIMALS"
echo "Total Supply:      $TOTAL_SUPPLY"
echo "Dry Run:           $DRY_RUN"
echo ""

# Construct commands
NEAR_CMD="near"
if [[ "$DRY_RUN" == "true" ]]; then
    echo -e "${YELLOW}=== DRY RUN MODE - Commands will be displayed but not executed ===${NC}\n"
fi

# Helper function to execute or print commands
run_cmd() {
    local cmd="$1"
    echo -e "${BLUE}Command:${NC} $cmd"
    if [[ "$DRY_RUN" == "false" ]]; then
        eval "$cmd"
        echo -e "${GREEN}✓ Success${NC}\n"
    else
        echo -e "${YELLOW}[DRY RUN - not executed]${NC}\n"
    fi
}

# In test mode, create subaccounts first
if [[ "$TEST_MODE" == "true" ]]; then
    echo -e "${GREEN}=== Test Mode: Creating Subaccounts ===${NC}"
    
    # Create controller subaccount
    echo -e "\n${BLUE}Creating controller account...${NC}"
    CREATE_CONTROLLER_CMD="$NEAR_CMD create-account $CONTROLLER_ID --masterAccount $SIGNER_ID --initialBalance $INITIAL_BALANCE --networkId $NETWORK"
    run_cmd "$CREATE_CONTROLLER_CMD"
    
    # Create token subaccount
    echo -e "\n${BLUE}Creating token account...${NC}"
    CREATE_TOKEN_CMD="$NEAR_CMD create-account $TOKEN_ID --masterAccount $SIGNER_ID --initialBalance $INITIAL_BALANCE --networkId $NETWORK"
    run_cmd "$CREATE_TOKEN_CMD"
else
    # In non-test mode, check if accounts exist and create if needed
    echo -e "${GREEN}=== Checking Account Existence ===${NC}"
    
    # Check controller account
    if [[ "$DRY_RUN" == "false" ]]; then
        CONTROLLER_EXISTS=$($NEAR_CMD state $CONTROLLER_ID --networkId $NETWORK 2>&1 | grep -q "Account" && echo "true" || echo "false")
    else
        CONTROLLER_EXISTS="unknown"
    fi
    
    if [[ "$CONTROLLER_EXISTS" == "false" ]]; then
        echo -e "${YELLOW}Controller account $CONTROLLER_ID does not exist${NC}"
        echo -e "\n${BLUE}Creating controller account...${NC}"
        CREATE_CONTROLLER_CMD="$NEAR_CMD create-account $CONTROLLER_ID --masterAccount $SIGNER_ID --initialBalance $INITIAL_BALANCE --networkId $NETWORK"
        run_cmd "$CREATE_CONTROLLER_CMD"
    elif [[ "$CONTROLLER_EXISTS" == "true" ]]; then
        echo -e "${GREEN}✓ Controller account $CONTROLLER_ID exists${NC}"
    else
        echo -e "${YELLOW}Skipping account existence check in dry-run mode${NC}"
    fi
    
    # Check token account
    if [[ "$DRY_RUN" == "false" ]]; then
        TOKEN_EXISTS=$($NEAR_CMD state $TOKEN_ID --networkId $NETWORK 2>&1 | grep -q "Account" && echo "true" || echo "false")
    else
        TOKEN_EXISTS="unknown"
    fi
    
    if [[ "$TOKEN_EXISTS" == "false" ]]; then
        echo -e "${YELLOW}Token account $TOKEN_ID does not exist${NC}"
        echo -e "\n${BLUE}Creating token account...${NC}"
        CREATE_TOKEN_CMD="$NEAR_CMD create-account $TOKEN_ID --masterAccount $SIGNER_ID --initialBalance $INITIAL_BALANCE --networkId $NETWORK"
        run_cmd "$CREATE_TOKEN_CMD"
    elif [[ "$TOKEN_EXISTS" == "true" ]]; then
        echo -e "${GREEN}✓ Token account $TOKEN_ID exists${NC}"
    else
        echo -e "${YELLOW}Skipping account existence check in dry-run mode${NC}"
    fi
fi

# Step 1: Deploy controller
echo -e "${GREEN}=== Step 1: Deploy Controller ===${NC}"
DEPLOY_CONTROLLER_CMD="$NEAR_CMD deploy $CONTROLLER_ID $CONTROLLER_WASM --initFunction new --initArgs '{\"dao\":\"$SIGNER_ID\"}' --networkId $NETWORK"

run_cmd "$DEPLOY_CONTROLLER_CMD"

# Step 2: Deploy token with signer as initial owner
echo -e "${GREEN}=== Step 2: Deploy Token with Signer as Initial Owner ===${NC}"
DEPLOY_TOKEN_CMD="$NEAR_CMD deploy $TOKEN_ID $TOKEN_WASM --initFunction new --initArgs '{\"owner_id\":\"$SIGNER_ID\",\"total_supply\":\"$TOTAL_SUPPLY\",\"metadata\":{\"spec\":\"ft-1.0.0\",\"name\":\"$TOKEN_NAME\",\"symbol\":\"$TOKEN_SYMBOL\",\"decimals\":$TOKEN_DECIMALS}}' --networkId $NETWORK"

run_cmd "$DEPLOY_TOKEN_CMD"

# Step 3: Transfer token ownership to controller
echo -e "${GREEN}=== Step 3: Transfer Token Ownership to Controller ===${NC}"
TRANSFER_OWNERSHIP_CMD="$NEAR_CMD call $TOKEN_ID owner_set '{\"new_owner\":\"$CONTROLLER_ID\"}' --accountId $SIGNER_ID --networkId $NETWORK"

run_cmd "$TRANSFER_OWNERSHIP_CMD"

# Step 4: Verify ownership
echo -e "${GREEN}=== Step 4: Verify Ownership ===${NC}"
VERIFY_CMD="$NEAR_CMD view $TOKEN_ID owner_get '{}' --networkId $NETWORK"

if [[ "$DRY_RUN" == "false" ]]; then
    echo -e "${BLUE}Verifying token owner...${NC}"
    OWNER_OUTPUT=$(eval "$VERIFY_CMD" 2>&1 || echo "")
    
    # Extract account ID - handle both quoted and unquoted output
    OWNER_RESULT=$(echo "$OWNER_OUTPUT" | grep -oE '(controller\.[a-z0-9\-\.]+|"[^"]+")' | tr -d '"' | tail -1)
    
    if [[ -n "$OWNER_RESULT" && "$OWNER_RESULT" == "$CONTROLLER_ID" ]]; then
        echo -e "${GREEN}✓ Ownership verified: $OWNER_RESULT${NC}\n"
    else
        echo -e "${RED}✗ Ownership verification failed.${NC}"
        echo -e "Expected: ${BLUE}$CONTROLLER_ID${NC}"
        echo -e "Got:      ${BLUE}$OWNER_RESULT${NC}"
        echo -e "Raw output: $OWNER_OUTPUT\n"
    fi
else
    echo -e "${BLUE}Command:${NC} $VERIFY_CMD"
    echo -e "${YELLOW}[DRY RUN - not executed]${NC}\n"
fi

# Summary
if [[ "$DRY_RUN" == "false" ]]; then
    echo -e "${GREEN}=== Deployment Complete ===${NC}"
    echo -e "Controller: ${BLUE}$CONTROLLER_ID${NC}"
    echo -e "Token:      ${BLUE}$TOKEN_ID${NC}"
    echo -e "Owner:      ${BLUE}$CONTROLLER_ID${NC}"
    
    # In test mode, clean up by deleting temporary accounts
    if [[ "$TEST_MODE" == "true" ]]; then
        echo -e "\n${YELLOW}=== Test Mode Cleanup ===${NC}"
        echo -e "Deleting temporary accounts with beneficiary: ${BLUE}$SIGNER_ID${NC}"
        
        # Delete token account first (must delete before controller since controller is owner)
        echo -e "\n${BLUE}Deleting token account...${NC}"
        DELETE_TOKEN_CMD="$NEAR_CMD delete-account $TOKEN_ID $SIGNER_ID --networkId $NETWORK"
        run_cmd "$DELETE_TOKEN_CMD"
        
        # Delete controller account
        echo -e "\n${BLUE}Deleting controller account...${NC}"
        DELETE_CONTROLLER_CMD="$NEAR_CMD delete-account $CONTROLLER_ID $SIGNER_ID --networkId $NETWORK"
        run_cmd "$DELETE_CONTROLLER_CMD"
        
        echo -e "${GREEN}✓ Cleanup complete - funds returned to $SIGNER_ID${NC}"
    else
        echo ""
        echo "Next steps:"
        echo "  1. Add release info to controller"
        echo "  2. Test pause/freeze via controller"
        echo "  3. Test upgrades via controller"
    fi
else
    if [[ "$TEST_MODE" == "true" ]]; then
        echo -e "\n${YELLOW}=== Test Mode Cleanup (Dry Run) ===${NC}"
        echo -e "${BLUE}Command:${NC} $NEAR_CMD delete-account $TOKEN_ID $SIGNER_ID --networkId $NETWORK"
        echo -e "${YELLOW}[DRY RUN - not executed]${NC}\n"
        echo -e "${BLUE}Command:${NC} $NEAR_CMD delete-account $CONTROLLER_ID $SIGNER_ID --networkId $NETWORK"
        echo -e "${YELLOW}[DRY RUN - not executed]${NC}\n"
    fi
    echo -e "${YELLOW}=== Dry Run Complete - No changes made ===${NC}"
fi
