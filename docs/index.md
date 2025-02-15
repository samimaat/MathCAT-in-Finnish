# MathCAT: Math Capable Assistive Technology
<img src="logo.png" style="position: relative; top: 16px; z-index: -1;">
is a library that supports conversion of MathML to:

* Speech strings with embedded speech engine commands
* Braille (Nemeth, UEB Technical, and eventually other braille math codes)
* Navigation of math (in multiple ways including overviews)

A goal of MathCAT is to be an easy to use library for screen readers and other assistive technology to use to produce high quality speech and/or braille from MathML. It is a follow-on project from MathPlayer (see below) and uses lessons learned from it to do to produce even higher quality speech, navigation, and braille. MathCAT takes advantage of some new ideas the [MathML Working Group](https://mathml-refresh.github.io/charter-drafts/math-2020.html) is developing to allow authors to express their intent when they use a notation. E.g., $(3, 6)$ could be a point in the plane or an open interval, or even a shorthand notation for the greatest common divisor. When that information is conveyed in the MathML, MathCAT will use it to generate more natural sounding speech.

Todo: incorporation of third party libraries to support a common subset of TeX math commands along with ASCIIMath.

MathCAT is written in Rust and can be built to interface with many languages. To date there are interfaces for:
* [C/C++](https://github.com/NSoiffer/MathCATForC)
* [Python](https://github.com/NSoiffer/MathCATForPython) -- this is used by an [NVDA add-on](https://addons.nvda-project.org/addons/MathCAT.en.html). I hope to eventually get it incorporated into [Orca](https://help.gnome.org/users/orca/stable) which is written in Python.
* [Java](https://github.com/mwhapples/MathCAT4J) -- this is currently being used to experiment with MathCAT in [BrailleBlaster](https://www.brailleblaster.org/).
* [WebAssembly (Wasm)](https://github.com/NSoiffer/MathCATDemo/) -- this is used for a web demo of MathCAT.

MathCAT uses a number of heuristics that try to repair poor MathML and put it in a recommended format. For example, TeX converters and WYSIWYG editors will take "1,234+1" and break the number "1,234" apart at the comma. MathCAT recognizes that and folds the number into a single `mn`. Other repairs are structural such as creating `mrow`s based on information from MathML's operator dictionary and adding invisible function application, multiplication, addition (mixed fractions), and separators (e.g, between the $i$ and $j$ in $a\_{ij}$) when it seems appropriate. This simplifies speech and Nemeth generation and may be useful to other apps. Currently the cleanup is not exposed in an API, but potentially it could be another service of MathCAT. In general, MathCAT is somewhat conservative in its repair. However, it likely will do the wrong thing in some cases, but the hope is it does the right thing much, much more frequently. Finding common mistakes of translators to MathML and patching up the poor MathML is an ongoing project.

## Current Status (updated 1/18/23)
MathCAT is under active development. Initial speech, navigation, and Nemeth generation is complete and [NVDA add-on](https://addons.nvda-project.org/addons/MathCAT.en.html) now exists. It should be usable as a MathPlayer replacement for those using the English version or one of the supported translations. It is not as complete or polished in some ways as MathPlayer though. However, it supports both Nemeth and UEB technical braille generation. The Nemeth braille is substantially better than that provided by MathPlayer and includes integration with navigation (uses dots 7 and 8 to indicate the navigation node).

A demo to show off some of MathCAT's features and also as an aid for debugging was developed. [Visit the demo](https://nsoiffer.github.io/MathCATDemo/) and please report any bugs you find. This demo is _not_ how AT users will typically interact with MathCAT but does show features that AT can potentially expose to end users such as highlighting of the speech, navigation, and braille.

Timeline (Starting Dec 2021):
* ✓ December/early January: prototype usage of preliminary MathML WG proposal for "intent"
* ✓ January: Distribute MathCAT to a small group of students and other users for feedback and bug reports
* ✓ February/March: Work on MathML->UEB translation
* ✓ April: Prosody implementation/compatibility with SAPI, One Core, eSpeak, and Eloquence voices
* late April/May: add more intent inference rules (ongoing)
* ✓ May: Release MathCAT as NVDA add-on
* ✓ June: C/C++ interface for MathCAT
* ✓ Late spring/summer: develop GUI interface for setting user preferences
* ✓ July - Oct: Add Chemistry-specific speech
* ✓ July/Aug/Sept: vacation 😎 and conference
* ✓ Nov/Dec: Work on at least one translation of MathCAT to another language (pushed back from late spring). Have Indonesian and Vietnamese translations.
* Winter, Spring 2023: add more inference/speech rules (at least units and currency)
* Winter, Spring 2023: analyze books to better determine what should be in the Unicode short file (hopefully get someone to help with this)
* Winter, Spring 2023: translation work
  * ✓ Create some tools to simplify generation of the Unicode files in different languages
  * Create some tools to help update other languages when the English version changes (adds new rules)
  * Work with translators and fix any problems they might turn up
  * Work with translators to hopefully add many languages
* Winter, Spring 2023: work on UEB->MathML translation and explore UEB->Nemeth math translator
* Summer 2023: potentially work on 2D Nemeth generation along with Nemeth input


These plans are very tentative and will likely change based on feedback from users and AT developers.
I also have commitments for working on the MathML spec, so that can also delay some of these dates.
A broken ankle also slowed me down in more ways than one in the spring and summer (2022).

# Documentation for different MathCAT Users

There are many different audiences for MathCAT and each audience has different interests/needs. Please see the following documentation for details based on your needs:
* AT users: [information about preferences you can set](users.md)
* AT developers/library users: [information about the API that MathCAT exposes](callers.md)
* Translators/Rule writers: [information about the files that need to be translated](helpers.md)
* MathCAT developers: information about MathCAT's design

## Why MathCAT?

MathCAT is a follow-on to MathPlayer. I developed MathPlayer's accessibility while at Design Science starting back in 2004 after I joined Design Science. At the time, MathPlayer was chiefly designed to be a C++ plugin to Internet Explorer (IE) that displayed MathML on web pages. For quite some time, it was the most complete MathML implementation available. The original work for display of math was done by Design Science's founder Paul Topping and their chief technology officer, the late Robert Miner. Eventually, for numerous reasons, IE withdrew the interface that MathPlayer used for display and did not implement a replacement as the world was moving towards using JavaScript in the browser and not allowing security threats posed by external code. This left MathPlayer as an accessibility-only library called by other programs (chiefly NVDA). MathPlayer was proprietary, but was given away for free.

In 2016, I left Design Science. In 2017, WIRIS bought Design Science. I volunteered to add bug fixes for free to MathPlayer and initially they were supportive of that. But when it came time to do a release, a number of the people around at the time of the buyout had left and the remaining team was not interested in supporting MathPlayer. That decision was not finalized until late 2020. In 2021, I started work on a replacement to MathPlayer. As a challenge, I decided to learn Rust and did the implementation in Rust. For those not familiar with Rust, it is a low level language that is type safe and memory safe, but not automatically garbage collected or reference counted. It is often touted as a safer replacement to C/C++.

Rust is quite efficient. On a Core I7-770K machine (higher end processor circa 2017), the moderate-size expression
<math xmlns="http://www.w3.org/1998/Math/MathML" display="block">
  <mrow>
    <msup>
      <mi>e</mi>
      <mrow>
        <mo>&#x2212;</mo>
        <mfrac>
          <mn>1</mn>
          <mn>2</mn>
        </mfrac>
        <msup>
          <mrow>
            <mrow>
              <mo>(</mo>
              <mrow>
                <mfrac>
                  <mrow>
                    <mi>x</mi>
                    <mo>&#x2212;</mo>
                    <mi>&#x03BC;</mi>
                  </mrow>
                  <mi>&#x03C3;</mi>
                </mfrac>
              </mrow>
              <mo>)</mo>
            </mrow>
          </mrow>
          <mn>2</mn>
        </msup>
      </mrow>
    </msup>
  </mrow>
</math>
takes about 4ms to generate the ClearSpeak string
"_e raised to the exponent, negative 1 half times; open paren; the fraction with numerator; x minus mu; and denominator sigma; close paren squared, end exponent_" along with the Nemeth braille string "⠑⠘⠤⠹⠂⠌⠆⠼⠈⠡⠷⠹⠭⠤⠨⠍⠌⠨⠎⠼⠾⠘⠘⠆".
This time is split approximately: 2ms to cleanup the MathML + 1ms for speech generation + 1ms for braille generation.
The MathML for this expression is:
```
<math>
  <mrow>
    <msup>
      <mi>e</mi>
      <mrow>
        <mo>&#x2212;</mo>
        <mfrac>
          <mn>1</mn>
          <mn>2</mn>
        </mfrac>
        <msup>
          <mrow>
            <mrow>
              <mo>(</mo>
              <mrow>
                <mfrac>
                  <mrow>
                    <mi>x</mi>
                    <mo>&#x2212;</mo>
                    <mi>&#x03BC;</mi>
                  </mrow>
                  <mi>&#x03C3;</mi>
                </mfrac>
              </mrow>
              <mo>)</mo>
            </mrow>
          </mrow>
          <mn>2</mn>
        </msup>
      </mrow>
    </msup>
  </mrow>
</math>
```

MathCAT uses external rules to generate speech and braille.
These take about 40ms to load; this load only happens the first time the rules are used, or if the speech style, language, or other external preference is changed. An additional 50ms are required to load the full Unicode files for speech and braille,
but studies have shown that a vast majority of English K-14 math material uses a surprisingly few number of characters.
Using open source math books, the initial load should cover at least 99.99% of the characters used in expressions encountered in English K-14 math textbooks.

The library is about ~3mb in size.

If you are working on an in-browser solution (i.e, you are using JavaScript or some other browser-based language), MathCAT is probably not the best tool for you (although I will probably add a Javascript interface). Instead, take a look at [Speech rule engine](https://github.com/zorkow/speech-rule-engine) (SRE) by Volker Sorge. It is written in TypeScript and will likely meet your needs for an in-browser solution.


# Acknowledgements
Several people helped out in various ways with the project. I am very grateful for all their help!

* David Carlisle -- provided invaluable help figuring out some xpath matches
* Susan Jolly -- provided lots of patient guidance on Nemeth and UEB generation along with feedback on what is right and wrong. On top of that, she also guided me as I tried to work out chemistry heuristics.
* Elaine A. Moore -- helped me to figure out what should and should not be said for chemistry, along with what makes sense as chemistry and what doesn't.
* Richard Orme -- did all the work for the MathCAT NVDA settings dialog.
* Murray Sargent and Volker Sorge -- provided tables of Nemeth translations of characters and Nemeth tests

Translators:
* Indonesian -- Dr. Pinta Deniyanti Sampoerno, M.Si; Dr. Meiliasari, S.Pd., M.Sc; and Ari Hendarno, S.Pd., M.kom
* Vietnamese -- Dang Hoai Phúc and Trang Pham
* Others??? -- please volunteer so I can list you here...

Thanks to everyone who volunteered!
