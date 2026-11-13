# ChaloTrace: Install and Launch Guide

Follow these steps in order. Total time: about 15 minutes, mostly waiting for downloads.
You install two things: the Rust toolchain and PostgreSQL (the database that saves your data). No Docker needed.

---

## Step 1. Install Rust

### Windows
1. Go to **https://rustup.rs** and download `rustup-init.exe`.
2. Run it. If it asks you to install the **Visual Studio C++ Build Tools**, say yes and let it finish
   (this is required on Windows; the download is a few GB).
3. When the installer menu appears, press **1** (default installation) and Enter.
4. **Close and reopen** your terminal (Command Prompt or PowerShell).

### Mac
1. Open **Terminal**.
2. Run: `curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh`
3. Press **1** for the default install, then close and reopen Terminal.
4. If asked to install Xcode command line tools, accept.

### Linux (Ubuntu/Debian)
1. Run: `sudo apt update && sudo apt install -y build-essential curl`
2. Run: `curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh`
3. Press **1**, then close and reopen the terminal.

**Check it worked.** In a new terminal run:
```
cargo --version
```
You should see something like `cargo 1.xx.x`. If you get "command not found", reopen the terminal
or restart the computer.

---

## Step 2. Get the project files

1. Unzip `ChaloTrace.zip` somewhere easy, for example `Documents`.
   - Windows: right-click the zip, **Extract All**.
   - Mac/Linux: `unzip ChaloTrace.zip`
2. Open a terminal **inside** the engine folder:
   - Windows: open the `rugged-ledger-core` folder in File Explorer, click the address bar, type `cmd`, press Enter.
   - Mac/Linux: `cd path/to/ChaloTrace/rugged-ledger-core`

Check you are in the right place: the folder should contain `Cargo.toml` and a `crates` folder.

---

## Step 3. Install PostgreSQL and create the database

The server saves every event to PostgreSQL so nothing is lost when it restarts.

### Linux (Ubuntu/Debian)
```
sudo apt update && sudo apt install -y postgresql
sudo -u postgres psql -c "CREATE USER chalotrace WITH PASSWORD 'chalotrace';" -c "CREATE DATABASE chalotrace OWNER chalotrace;"
```
(That password is for your own laptop only. Use a strong, different one on any real server.)

### Mac (with Homebrew, from https://brew.sh)
```
brew install postgresql@16
brew services start postgresql@16
psql postgres -c "CREATE USER chalotrace WITH PASSWORD 'chalotrace';" -c "CREATE DATABASE chalotrace OWNER chalotrace;"
```

### Windows
1. Download the installer from **https://www.postgresql.org/download/windows/** and run it. Keep the default port **5432**, and write down the password you set for the `postgres` user.
2. From the Start menu open **SQL Shell (psql)**. Press Enter through the questions, then enter your `postgres` password.
3. Paste:
```
CREATE USER chalotrace WITH PASSWORD 'chalotrace';
CREATE DATABASE chalotrace OWNER chalotrace;
```

Tables are created automatically the first time the server starts. You never run the SQL file by hand.

---

## Step 4. Run the tests (optional but recommended)

```
cargo test --workspace
```

The first run downloads and compiles libraries, which takes a few minutes. At the end you should see
`test result: ok` several times and no failures. This proves the signing and verification logic works
on your machine.

To also run the PostgreSQL integration tests against the local database created in Step 3:
```
export TEST_DATABASE_URL="postgres://chalotrace:chalotrace@localhost/chalotrace"
cargo test -p ledger-server --test postgres
```
These tests create a separate temporary schema for each test and remove it afterward. If
`TEST_DATABASE_URL` is unset, the PostgreSQL integration tests skip without connecting.
The example credentials are for local development only.

---

## Step 5. Launch the server (saving to PostgreSQL)

Tell the server where the database is, then start it.

**Linux / Mac:**
```
export DATABASE_URL=postgres://chalotrace:chalotrace@localhost/chalotrace
export ADMIN_TOKEN="replace-with-a-long-random-admin-token"
cargo run -p ledger-server
```
**Windows (PowerShell):**
```
$env:DATABASE_URL="postgres://chalotrace:chalotrace@localhost/chalotrace"
$env:ADMIN_TOKEN="replace-with-a-long-random-admin-token"
cargo run -p ledger-server
```
`ADMIN_TOKEN` enables the organization/device administration API. If it is unset, all admin
routes are disabled. Keep the token private; the example value above is a placeholder, not a
production credential. A genesis signing key must be enrolled before its first event is accepted.
Handoff to an unenrolled key is allowed; the public verification output flags that custodian.

Wait until you see:
```
Connected to PostgreSQL; loaded and verified 0 saved products
ChaloTrace ledger-server listening on 0.0.0.0:8080
```
(If you forget `DATABASE_URL`, the server still starts but prints a WARNING that it is running in memory and will lose data when stopped.)

**Leave this terminal window open.** Stop it any time with **Ctrl + C**.

Quick check: open **http://localhost:8080/health** in your browser. It should say `ok`.

*Port 8080 already in use?* Pick another port. Windows (PowerShell): `$env:LISTEN_ADDR="0.0.0.0:9090"; cargo run -p ledger-server`.
Mac/Linux: `LISTEN_ADDR=0.0.0.0:9090 cargo run -p ledger-server`.

---

## Step 6. See it work with the demo shipment

Open a **second** terminal in the same `rugged-ledger-core` folder (as in Step 2) and run:

```
cargo run -p ledger-server --example seed_demo
```

This simulates a honey batch: created by a farmer in Mwinilunga, handed to a truck driver, and
recorded arriving at a Lusaka warehouse. All three signed events go to the server in one request.
You should see:
```
Server replied: 3 accepted, 0 duplicates, 0 rejected
```
and then a link like:
```
http://127.0.0.1:8080/api/v1/verify/1bb168022f4579abc11e06ab2b7cb07b
```
**Copy that link into your browser.** You will see the verified timeline: each event, who signed it,
the GPS location, the note, and `"verified": true`.

Each time you run the demo you get a new batch with a new link.

**Prove the data is saved:** stop the server with Ctrl + C, start it again (Step 5), and reopen your link. The history is still there, and the startup line now says `loaded and verified 1 saved products`. Every saved chain is re-checked from its signatures each time the server starts; if anyone altered the database, the server refuses to start and names the damaged product.

---

## Step 7. Put the code on GitHub (optional)

1. On github.com, create a new **empty** repository named `rugged-ledger-core` (no README, no license).
2. In the `rugged-ledger-core` folder terminal:
```
git remote add origin https://github.com/YOBAH8/rugged-ledger-core.git
git branch -M main
git push -u origin main
```
(You need Git installed from https://git-scm.com. The folder already contains one commit.)

---

## Troubleshooting

| Problem | Fix |
|---|---|
| `cargo: command not found` | Close and reopen the terminal; on Windows make sure the C++ Build Tools finished installing. |
| `linker 'link.exe' not found` (Windows) | Install **Visual Studio Build Tools** with the "Desktop development with C++" option, then retry. |
| `Could not reach the server` when running the demo | The server from Step 5 is not running. Start it first and keep that window open. |
| `address already in use` | Another program uses port 8080. Use the alternative port shown in Step 5 (and set `SERVER_ADDR=127.0.0.1:9090` for the demo). |
| First build is very slow | Normal. Later builds take seconds. |
| Browser shows "unknown product" | You started the server without `DATABASE_URL` (memory only), so the batch was lost on restart. Set it and run the demo again. |
| `password authentication failed` | The user/password in `DATABASE_URL` does not match what you created in Step 3. |
| `cannot connect to PostgreSQL ... Connection refused` | PostgreSQL is not running. Linux: `sudo systemctl start postgresql`. Mac: `brew services start postgresql@16`. Windows: start the "postgresql" service. |
| `saved chain ... FAILED verification` | A stored event was modified after signing. Do not ignore this; the data in that database cannot be trusted. |
