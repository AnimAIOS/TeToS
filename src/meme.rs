pub const BANNER: &str = r#"
 ██████╗ ██████╗ ███████╗████████╗ ██████╗ ███████╗
 ██╔═══██╗██╔══██╗██╔════╝╚══██╔══╝██╔═══██╗██╔════╝
 ██║   ██║██║  ██║███████╗   ██║   ██║   ██║███████╗
 ██║   ██║██║  ██║╚════██║   ██║   ██║   ██║╚════██║
 ╚██████╔╝██████╔╝███████║   ██║   ╚██████╔╝███████║
  ╚═════╝ ╚═════╝ ╚══════╝   ╚═╝    ╚═════╝ ╚══════╝
   Teto as a Service — runs on any poTETO PC, zero GPUs, maximum diva.
 "#;

pub fn startup_lines() -> &'static [&'static str] {
    &[
        "Boot sequence: insert haircut memory card... done.",
        "Hardware check: poTETO PC detected. The diva approves of this potato.",
        "No neural voice cloning detected. The samples are authentic UTAU.",
        "Billing: $0.00. Rights: read the voicebank license. Vibes: immaculate.",
        "OpenAI clients may now set base_url and speak to a diva.",
        "Reminder: this replaces the TTS bill, not the LLM. She talks, she does not reason.",
        "April Fools 2011: still not a VOCALOID. Still the diva.",
        "If the audio sounds cursed, that is a feature called 'concatenative synthesis'.",
        "The diva takes no acting notes, but she will nod politely at your instructions.",
    ]
}
