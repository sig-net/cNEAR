pub mod common;

use near_sdk::json_types::U128;

use common::{init_accounts, init_contracts, TOTAL_SUPPLY};

#[tokio::test]
async fn test_total_supply() -> anyhow::Result<()> {
    let initial_balance = TOTAL_SUPPLY;

    let worker = near_workspaces::sandbox().await?;
    let root = worker.root_account()?;
    let (alice, _, _, _) = init_accounts(&root).await?;
    let (ft_contract, _) = init_contracts(&worker, initial_balance, &alice).await?;

    let res = ft_contract.call("ft_total_supply").view().await?;
    assert_eq!(res.json::<U128>()?, initial_balance);

    Ok(())
}
