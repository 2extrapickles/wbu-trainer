WBU Trainer 1.2.2 - for Wakeboarding Unleashed (PC, Aspyr 2003)
=================================================================

A small trainer with three tabs:

  Live cheats   Score set / add, score multiplier, endless combo, combo multiplier,
                unbreakable rope (you can still drop it), infinite rope grab range,
                no crashing (no falls, no respawn), no speed loss while grinding,
                no speed loss with the rope off, double jump anywhere (uses the game's
                jump key, unlimited air jumps; key and power adjustable).
                Hotkeys (numeric keypad, work while the game has focus):
                  Num 1  score multiplier on/off
                  Num 2  endless combo on/off
                  Num 3  combo multiplier on/off
                  Num 4  +10,000 score
                  Num 5  unbreakable rope on/off
                  Num 6  no speed loss while grinding on/off
                  Num 7  no speed loss with the rope off on/off
                  Num 8  infinite rope grab range on/off
                  Num 9  no crashing on/off
                  Num 0  Florida & The Delta in free ride on/off
  Models        Live, while you play (needs the helper, see below):
                - your rider can look like any model in the game: another rider,
                  a gator, a gondolier, a cow... (non-riders ride stiffly on the board)
                - your boat can look like any model too (gondola, jet ski, orca...)
                - big heads, rider size (for non-rider looks), boat size, turn
                - the cut "Secret" level, loaded in place of a level you pick
                  (applies the next time you enter that level)
  Save editor   Career saves (game closed). A backup is made before every write
                (Game\Save\wbu-trainer-backup\).
                - Unlock everything in one click (what the game's own Ultimate cheat does,
                  for every rider), or one by one: boards, secret boarders, videos,
                  perfect balance, free-ride levels
                - per rider: stats (max 10) and 100% career (every goal, challenge and gap,
                  every level beaten; career levels and the credits level unlock from it)

Install
-------
1. Put wbu-trainer.exe anywhere (for example in the game's "Game" folder).
2. Run it. It finds the default install; otherwise pick the folder that contains WBU.EXE.
3. Models tab: click "Install" to copy dinput8.dll into the game folder.
   (Only needed for the Models tab. Live cheats and the save editor work without it.
   Install it before starting the game; after that, changes apply instantly.)
4. Start the game (from the trainer or as usual). Keep the trainer open for live cheats.

Uninstall: delete dinput8.dll, wbu-trainer.ini and wbu-trainer.log from the game folder.

Notes
-----
- Works with the Aspyr PC release (WBU.EXE). Other versions are detected and left alone.
- The score on screen catches up after your next trick.
- Compatible with the WBU widescreen fix (d3d8.dll); both can be installed together.
- The game folder lives under Program Files: if saving settings fails, run the trainer
  as administrator once, or give your user write access to the game folder.
- Close the game before editing saves.
- Cheats that change the game's code (rope, grab range, no crashing, score multiplier) stay
  active until you switch them off or restart the game, even if the trainer is closed.
- The trainer also fixes the game's "realloc relocated memory block!" crash on level change / quit
  (in dinput8.dll).

Coming later: moon gravity / giant air, boat speed, infinite boost, instant special.

MIT license, see LICENSE.txt.
