use burn::{
    config::Config,
    module::Module,
    record::{DefaultRecorder, Recorder, RecorderError},
    tensor::{
        self,
        backend::{self, Backend},
        Data, Float, Int, Tensor,
    },
};
use burn_wgpu::{AutoGraphicsApi, WgpuBackend, WgpuDevice};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use hound::{self, SampleFormat};
use num_traits::ToPrimitive;
use anyhow::{Error as E, Result};
use strum::IntoEnumIterator;
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    env, fs, iter, process
};
use whisper::{
    audio::prep_audio,
    model::*,
    helper::*,
    token::{Gpt2Tokenizer, SpecialToken},
    transcribe::waveform_to_text,
    token, token::Language
};

//inference device backend
type IDBackend = WgpuBackend<AutoGraphicsApi, f32, i32>;
fn main() {
    //COMMAND LINE
    let (model_name, wav_file, text_file, lang) = parse_args();

    let device = WgpuDevice::BestAvailable;
    let (bpe, whisper_config, whisper) = load_model(&model_name, &device);

    //START AUDIO SERVER
    // Set up the input device and stream with the default input config.
    let audio_host = cpal::default_host();
    let audio_device = audio_host
        .default_input_device()
        .expect("Failed to get default input device");

    let audio_config = audio_device
        .default_input_config()
        .expect("Failed to get default input config");

    let channel_count = audio_config.channels() as usize;

    let audio_ring_buffer = Arc::new(Mutex::new(Vec::new()));
    let audio_ring_buffer_2 = audio_ring_buffer.clone();

    std::thread::spawn(move || loop {
        let data = record_audio(&audio_device, &audio_config, 300).unwrap();
        audio_ring_buffer.lock().unwrap().extend_from_slice(&data);
        let max_len = data.len() * 16;
        let data_len = data.len();
        let len = audio_ring_buffer.lock().unwrap().len();
        if len > max_len {
            let mut data = audio_ring_buffer.lock().unwrap();
            let new_data = data[data_len..].to_vec();
            *data = new_data;
        }
    });

    // loop to process the audio data forever (until the user stops the program)
    println!("Transcribing audio...");
    for (i, _) in iter::repeat(()).enumerate() {
        std::thread::sleep(std::time::Duration::from_millis(3000));
        let data = audio_ring_buffer_2.lock().unwrap().clone();
        let pcm_data: Vec<_> = data[..data.len() / channel_count as usize]
            .iter()
            .map(|v| *v as f32 / 32768.)
            .collect();

        //RUN INFERENCE
        let (text, tokens) = match waveform_to_text(&whisper, &bpe, lang, pcm_data, 16000) {
            Ok((text, tokens)) => (text, tokens),
            Err(e) => {
                eprintln!("Error during transcription: {}", e);
                process::exit(1);
            }
        };
        println!("{:?}", text);
    }
}

fn parse_args() -> (String, String, String, Language) {
    let args: Vec<String> = env::args().collect();

    if args.len() < 5 {
        eprintln!(
            "Usage: {} <model name> <audio file> <lang> <transcription file>",
            args[0]
        );
        process::exit(1);
    }

    let model_name = args[1].clone();
    let wav_file = args[2].clone();
    let text_file = args[4].clone();

    let lang_str = &args[3];
    let lang = match Language::iter().find(|lang| lang.as_str() == lang_str) {
        Some(lang) => lang,
        None => {
            eprintln!("Invalid language abbreviation: {}", lang_str);
            process::exit(1);
        }
    };

    (model_name, wav_file, text_file, lang)
}

fn load_whisper_model_file<B: Backend>(
    config: &WhisperConfig,
    model_name: &str,
) -> Result<Whisper<B>, RecorderError> {
    DefaultRecorder::new()
        .load(format!("models/{}/{}", model_name, model_name).into())
        .map(|record| config.init().load_record(record))
}

fn load_model(
    model_name: &str,
    device: &WgpuDevice,
) -> (Gpt2Tokenizer, WhisperConfig, Whisper<IDBackend>) {
    let bpe = match Gpt2Tokenizer::new(&model_name) {
        Ok(bpe) => bpe,
        Err(e) => {
            eprintln!("Failed to load tokenizer: {}", e);
            process::exit(1);
        }
    };

    let whisper_config =
        match WhisperConfig::load(&format!("models/{}/{}.cfg", &model_name, &model_name)) {
            Ok(config) => config,
            Err(e) => {
                eprintln!("Failed to load whisper config: {}", e);
                process::exit(1);
            }
        };

    println!("Loading model...");
    let whisper: Whisper<IDBackend> = match load_whisper_model_file(&whisper_config, &model_name) {
        Ok(whisper_model) => whisper_model,
        Err(e) => {
            eprintln!("Failed to load whisper model file: {}", e);
            process::exit(1);
        }
    };

    let whisper = whisper.to_device(&device);

    (bpe, whisper_config, whisper)
}

fn record_audio(
    device: &cpal::Device,
    config: &cpal::SupportedStreamConfig,
    milliseconds: u64,
) -> Result<Vec<i16>> {
    let writer = Arc::new(Mutex::new(Vec::new()));
    let writer_2 = writer.clone();
    let stream = device.build_input_stream(
        &config.config(),
        move |data: &[f32], _: &cpal::InputCallbackInfo| {
            let processed = data
                .iter()
                .map(|v| (v * 32768.0) as i16)
                .collect::<Vec<i16>>();
            writer_2.lock().unwrap().extend_from_slice(&processed);
        },
        move |err| {
            eprintln!("an error occurred on stream: {}", err);
        },
        None,
    )?;
    stream.play()?;
    std::thread::sleep(std::time::Duration::from_millis(milliseconds));
    drop(stream);
    let data = writer.lock().unwrap().clone();
    let step = 3;
    let data: Vec<i16> = data.iter().step_by(step).copied().collect();
    //println!("{:?}", data);
    Ok(data)
}
