pub const BANNER: &str = r#"
 ██████╗ ██████╗ ███████╗████████╗ ██████╗ ███████╗
 ██╔═══██╗██╔══██╗██╔════╝╚══██╔══╝██╔═══██╗██╔════╝
 ██║   ██║██║  ██║███████╗   ██║   ██║   ██║███████╗
 ██║   ██║██║  ██║╚════██║   ██║   ██║   ██║╚════██║
 ╚██████╔╝██████╔╝███████║   ██║   ╚██████╔╝███████║
  ╚═════╝ ╚═════╝ ╚══════╝   ╚═╝    ╚═════╝ ╚══════╝
   Teto as a Service — free voice, zero GPUs, maximum diva.
"#;

pub fn startup_lines() -> [&'static str; 6] {
    [
        "Boot sequence: insert haircut memory card... done.",
        "No neural voice cloning detected. The samples are authentic UTAU.",
        "Billing: $0.00. Rights: read the voicebank license. Vibes: immaculate.",
        "OpenAI clients may now set base_url and speak to a diva.",
        "Reminder: this replaces the TTS bill, not the LLM. She talks, she does not reason.",
        "If the audio sounds cursed, that is a feature called 'concatenative synthesis'.",
    ]
}
