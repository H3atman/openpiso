# Wire and set up the coin slot

::: warning Not yet checked on real hardware
The header pin numbers and the coin teaching steps on this page are waiting for the maintainer to confirm them on a real coin slot. Check them against your motherboard manual and your coin slot's manual before you wire.
:::

On this page you connect the coin slot to the Server PC, set its two side switches, and teach it your coins. Any Allan or generic multi-coin slot works.

::: tip
Your coin slot's own manual wins where it differs from this page. Button names, switch labels and wire colors are not the same on every unit.
:::

## Safety first

Switch off the Server PC and unplug its power cord before you touch any wire. Keep it unplugged until every step under "Wire the coin slot" is done.

## The wiring diagram

![Coin slot wiring: 12 V and GND from the Server PC's Molex connector to the coin slot, the blue COIN wire split to header pins 4 and 6, the green Set wire to header pin 7, and the two side switches set to NO and Slow](/images/coin-slot-wiring.svg)

The diagram shows the COM header as most motherboards lay it out: two rows of pins, with pin 1 at the bottom left. The top row holds the even pins (2, 4, 6, 8) and one empty spot where pin 10 would be. The bottom row holds the odd pins (1, 3, 5, 7, 9).

## Set the two side switches

The coin slot has two small slide switches on its side.

1. Set the **NO / NC** switch to **NO**.
2. Set the **Fast / Medium / Slow** switch to **Slow**.

OpenPiso counts coins wrongly if either switch is in another position.

## Wire the coin slot

1. Open the Server PC's case and find a free Molex connector on the power supply. It is the flat, four-pin plug with yellow, black, black and red wires.
2. Connect the Molex **yellow** wire (12 V) to the coin slot's **DC12V** wire. On most coin slots this wire is red.
3. Connect a Molex **black** wire (ground) to the coin slot's **GND** wire. Do not use the Molex red wire. It carries 5 V, not 12 V.
4. Find the COM header on the motherboard. It is a block of 9 pins in two rows, usually labelled **JCOM1** or **COM1**. Check your motherboard manual to confirm which pin is pin 1.
5. Split the coin slot's **COIN** wire (blue in the diagram) into two, like the letter Y. Connect one end to **pin 4** and the other end to **pin 6**. Pin 4 keeps the coin signal steady. Pin 6 is the one OpenPiso reads, called the **Coin Pin**.
6. The coin slot's **Set** lead has two wires. Connect the **green** wire to **pin 7**. This is the **Set Pin**, which OpenPiso uses to switch the coin slot on and off.
7. Do not connect the **grey** Set wire. Cover its bare end with electrical tape so it cannot touch anything.
8. Check every connection against the diagram. Make sure no bare wire touches another wire or the case.
9. Close the case, plug the Server PC back in, and switch it on.

The coin slot's display should light up when the Server PC is on.

## Teach the coin slot your coins

::: warning Not yet checked on real hardware
These teaching steps are drafted from the usual procedure for multi-coin slots. They are waiting for the maintainer to confirm them on a real coin slot. If your coin slot's manual says something different, follow the manual.
:::

The coin slot must learn what each coin looks like, and how many pulses to send for it. OpenPiso counts **one peso for every pulse**, so set each coin to send one pulse per peso:

| Coin | Pulses |
| ---- | ------ |
| ₱1   | 1      |
| ₱5   | 5      |
| ₱10  | 10     |
| ₱20  | 20     |

The buttons on the back of the coin slot are often labelled **ADD**, **MINUS** and **SET**. Yours may use other names.

### Enter the settings

1. With the Server PC on, press and hold **ADD** and **MINUS** together until the display changes. It now shows the first setting.
2. Set the **number of coin types** to **4**, using **ADD** and **MINUS**. Press **SET** to save it and move on.

### Set each coin

The coin slot now walks through each coin type in turn, starting with the first one. Use it for ₱1, the second for ₱5, the third for ₱10 and the fourth for ₱20. For each coin it asks for three values. Use **ADD** and **MINUS** to change a value, and **SET** to save it and move on.

1. **Sample count**: how many sample coins you will insert for this coin, for example 15.
2. **Pulse count**: the number of pulses from the table above. For ₱20, set 20.
3. **Accuracy**: leave the value the coin slot suggests, unless its manual says otherwise. On most units a smaller number is stricter.

When all four coins are set, the display leaves the settings.

### Insert the sample coins

1. Press and hold **SET** until the display shows that it is ready for samples. It usually shows the first coin type.
2. Insert the number of ₱1 coins you chose as the sample count, one at a time. Use different coins, old and new, so the coin slot learns how real coins vary.
3. When the coin slot moves to the next coin type, insert the ₱5 samples. Then do the same for ₱10 and ₱20.
4. When the last sample is in, the coin slot saves what it learned and leaves sample mode.

Once OpenPiso is running, the coin slot returns every coin until a customer picks a PC. This is normal. You test real coins on the next page.

You should now see the coin slot's display lit, with the coin slot wired to pins 4, 6 and 7 of the COM header and taught all four coins.

Next: [Connect and test the coin slot](/coin-slot-settings)
