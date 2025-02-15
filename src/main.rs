// *** MathCAT doesn't normally want to build a binary ***
// *** This file is here because it is useful for trying out things ***

// Maybe also have this speak to test the TTS generation.
// There is a rust winapi crate that mirrors the WinPAI and has "Speak(...)" in it
fn main() {
  use libmathcat::interface::*;
  use log::*;
  use std::time::Instant;
  env_logger::builder()
      .format_timestamp(None)
      .format_module_path(false)
      .format_indent(None)
      .format_level(false)
      .init();

  //  let expr = "
  //     <math display='block' xmlns='http://www.w3.org/1998/Math/MathML'>
  //     <mrow>
  //      <mrow><mo>[</mo>
  //        <mtable>
  //         <mtr>
  //          <mtd>
  //           <mn>3</mn>
  //          </mtd>
  //          <mtd>
  //           <mn>1</mn>
  //          </mtd>
  //          <mtd>
  //           <mn>4</mn>
  //          </mtd>
  //         </mtr>
  //         <mtr>
  //          <mtd>
  //           <mn>0</mn>
  //          </mtd>
  //          <mtd>
  //           <mn>2</mn>
  //          </mtd>
  //          <mtd>
  //           <mn>6</mn>
  //          </mtd>
  //         </mtr>','
  //        </mtable>
  //      <mo>]</mo></mrow></mrow>
  //    </math>
  // ";

  // let expr = "<math display='inline' xmlns='http://www.w3.org/1998/Math/MathML'>
  //       <msup intent='power($base(2, $base),silly($exp,-1.))'>
  //       <mi arg='base'>x</mi>
  //       <mi arg='exp'>n</mi>
  //     </msup>
  //       </math>
  //     ";
  // let expr = "<mrow intent='pre@prefix(in@infix($a, x))(post@postfix($b))'>
  //     <mi arg='a'>A</mi>
  //     <mover>
  //         <mo intent='map'>⟶</mo>
  //         <mo intent='congruence'>≅</mo>
  //     </mover>
  //     <mi arg='b'>B</mi>
  //   </mrow>";
  // let expr = "<math><mi>Na</mi><mi>S</mi><mo>(</mo><mi>l</mi><mo>)</mo></math>";


  // let expr = "<math xmlns='http://www.w3.org/1998/Math/MathML' display='block'>
  //     <mrow>
  //       <mo stretchy='false'>[</mo>
  //       <mrow>
  //         <mi>Co</mi>
  //       </mrow>
  //       <mo stretchy='false'>(</mo>
  //       <mrow>
  //         <mi>NH</mi>
  //       </mrow>
  //       <msub>
  //         <mrow>
  //           <mrow>
  //             <mpadded width='0'>
  //               <mphantom>
  //                 <mi>A</mi>
  //               </mphantom>
  //             </mpadded>
  //           </mrow>
  //         </mrow>
  //         <mrow>
  //           <mrow>
  //             <mpadded height='0'>
  //               <mn>3</mn>
  //             </mpadded>
  //           </mrow>
  //         </mrow>
  //       </msub>
  //       <mo stretchy='false'>)</mo>
  //       <msub>
  //         <mrow>
  //           <mrow>
  //             <mpadded width='0'>
  //               <mphantom>
  //                 <mi>A</mi>
  //               </mphantom>
  //             </mpadded>
  //           </mrow>
  //         </mrow>
  //         <mrow>
  //           <mrow>
  //             <mpadded height='0'>
  //               <mn>6</mn>
  //             </mpadded>
  //           </mrow>
  //         </mrow>
  //       </msub>
  //       <mo stretchy='false'>]</mo>
  //       <msup>
  //         <mrow>
  //           <mrow>
  //             <mpadded width='0'>
  //               <mphantom>
  //                 <mi>A</mi>
  //               </mphantom>
  //             </mpadded>
  //           </mrow>
  //         </mrow>
  //         <mrow>
  //           <mn>3</mn>
  //           <mo>+</mo>
  //         </mrow>
  //       </msup>
  //       <mtext>&#xA0;</mtext>
  //       <mo stretchy='false'>(</mo>
  //       <mrow>
  //         <mi>Cl</mi>
  //       </mrow>
  //       <msub>
  //         <mrow>
  //           <mrow>
  //             <mpadded width='0'>
  //               <mphantom>
  //                 <mi>A</mi>
  //               </mphantom>
  //             </mpadded>
  //           </mrow>
  //         </mrow>
  //         <mrow>
  //           <mrow>
  //             <mpadded height='0'>
  //               <mn>3</mn>
  //             </mpadded>
  //           </mrow>
  //         </mrow>
  //       </msub>
  //       <mo stretchy='false'>)</mo>
  //       <msup>
  //         <mrow>
  //           <mrow>
  //             <mpadded width='0'>
  //               <mphantom>
  //                 <mi>A</mi>
  //               </mphantom>
  //             </mpadded>
  //           </mrow>
  //         </mrow>
  //         <mrow>
  //           <mo>&#x2212;</mo>
  //         </mrow>
  //       </msup>
  //     </mrow>
  //   </math>";
  let expr="<math>
  <mn>23</mn>
  <mi intent='-3'>aaaaaa</mi>
  </math>";
//   let expr = "
//   <math display='block'>
//   <mrow displaystyle='true' data-changed='added'>
//     <mrow data-changed='added'>
//       <mi>A</mi>
//       <mo data-changed='added'>&#x2062;</mo>
//       <mi>x</mi>
//     </mrow>
//     <mo>+</mo>
//     <mi>b</mi>
//   </mrow>
//  </math>
//     ";
  // let expr= "<math><mrow><mfrac><mn>1</mn><mn>3</mn></mfrac><mo ame-texclass='bin' stretchy='false'>&#x22C5;</mo><mfrac><mn>85</mn><mn>124</mn></mfrac><mo ame-texclass='bin' stretchy='false'>+</mo><mfrac><mn>2</mn><mn>7</mn></mfrac><mo ame-texclass='bin' stretchy='false'>&#x22C5;</mo><mfrac><mn>39</mn><mn>124</mn></mfrac><mo ame-texclass='bin' stretchy='false'>+</mo><mfrac><mn>5</mn><mn>7</mn></mfrac></mrow></math>";
  let instant = Instant::now();
  let rules_dir = std::env::current_exe().unwrap().parent().unwrap().join("../../../Rules");
  let rules_dir = rules_dir.as_os_str().to_str().unwrap().to_string();
  if let Err(e) = set_rules_dir(rules_dir) {
    panic!("Error: exiting -- {}", errors_to_string(&e));  }

  info!("Version = '{}'", get_version());
  set_preference("Language".to_string(), "en".to_string()).unwrap();
  set_preference("TTS".to_string(), "none".to_string()).unwrap();
  set_preference("Verbosity".to_string(), "Medium".to_string()).unwrap();
  set_preference("Impairment".to_string(), "Blindness".to_string()).unwrap();
  // set_preference("SpeechOverrides_CapitalLetters".to_string(), "".to_string()).unwrap();
  // set_preference("CapitalLetters_UseWord".to_string(), "true".to_string()).unwrap();
  // set_preference("CapitalLetters_Pitch".to_string(), "30".to_string()).unwrap();
  set_preference("CapitalLetters_Beep".to_string(), "true".to_string()).unwrap();
  // set_preference("MathRate".to_string(), "77".to_string()).unwrap();
  
  set_preference("Bookmark".to_string(), "false".to_string()).unwrap();
  set_preference("SpeechStyle".to_string(), "ClearSpeak".to_string()).unwrap();
  if let Err(e) = set_mathml(expr.to_string()) {
    panic!("Error: exiting -- {}", errors_to_string(&e));
  };

  match get_spoken_text() {
    Ok(speech) => info!("Computed speech string:\n   '{}'", speech),
    Err(e) => panic!("{}", errors_to_string(&e)),
  }
  info!("SpeechStyle: {:?}", get_preference("SpeechStyle".to_string()).unwrap());
 
  // set_preference("BrailleCode".to_string(), "Nemeth".to_string()).unwrap();
  // match get_braille("".to_string()) {
  //   Ok(braille) => info!("Computed braille string:\n   '{}'", braille),
  //   Err(e) => panic!("{}", errors_to_string(&e)),
  // }

  info!("Time taken for loading+speech+braille: {}ms", instant.elapsed().as_millis());
  // let instant = Instant::now();
  // match get_spoken_text() {
  //   Ok(speech) => info!("Computed speech string:\n   '{}'", speech),
  //   Err(e) => panic!("{}", errors_to_string(&e)),
  // }
  // info!("Time taken (second time for speech): {}ms", instant.elapsed().as_millis());
  // info!("SpeechStyle: {:?}", get_preference("SpeechStyle".to_string()));
  
  // match get_braille("".to_string()) {
  //   Ok(braille) => info!("Computed braille string:\n   '{}'", braille),
  //   Err(e) => panic!("{}", errors_to_string(&e)),
  // }
  // // let xpath_counts = libmathcat::speech::xpath_count();
  // // info!("#xpath = {}; duplicates = {}", xpath_counts.0, xpath_counts.1);
  // info!("Time taken (second time for speech + braille): {}ms", instant.elapsed().as_millis());
}
