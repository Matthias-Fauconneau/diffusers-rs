// Stable diffusion inpainting pipeline.
// See the main stable-diffusion example for how to get the weights.
//
// This has been mostly adapted from looking at the diff between the sample
// diffusion standard and inpaint pipelines in the diffusers library.
// patdiff src/diffusers/pipelines/stable_diffusion/pipeline_stable_diffusion{,_inpaint}.py
//
// Sample input image:
// https://raw.githubusercontent.com/CompVis/latent-diffusion/main/data/inpainting_examples/overture-creations-5sI6fQgYIuo.png
// Sample mask:
// https://raw.githubusercontent.com/CompVis/latent-diffusion/main/data/inpainting_examples/overture-creations-5sI6fQgYIuo_mask.png
use clap::Parser;
use diffusers::{pipelines::stable_diffusion, schedulers::ddim, transformers::clip};
use tch::{nn::Module, Device, Kind, Tensor};

const HEIGHT: i64 = 512;
const WIDTH: i64 = 512;
const GUIDANCE_SCALE: f64 = 7.5;

#[derive(Parser)]
#[command(author, version, about, long_about = None)]
struct Args {
    /// The input image that will be inpainted.
    #[arg(long, value_name = "FILE")]
    input_image: String,

    /// The mask image to be used for inpainting, white pixels are repainted whereas black pixels
    /// are preserved.
    #[arg(long, value_name = "FILE")]
    mask_image: String,

    /// The prompt to be used for image generation.
    #[arg(long, default_value = "A fantasy landscape, trending on artstation.")]
    prompt: Option<String>,

    /// When set, use the CPU for the listed devices, can be 'all', 'unet', 'clip', etc.
    /// Multiple values can be set.
    #[arg(long)]
    cpu: Vec<String>,

    /// The UNet weight file, in .ot format.
    #[arg(long, value_name = "FILE", default_value = "data/unet.ot")]
    unet_weights: String,

    /// The CLIP weight file, in .ot format.
    #[arg(long, value_name = "FILE", default_value = "data/pytorch_model.ot")]
    clip_weights: String,

    /// The VAE weight file, in .ot format.
    #[arg(long, value_name = "FILE", default_value = "data/vae.ot")]
    vae_weights: String,

    /// The size of the sliced attention or 0 to disable slicing (default)
    #[arg(long)]
    sliced_attention_size: Option<i64>,

    /// The number of steps to run the diffusion for.
    #[arg(long, default_value_t = 30)]
    n_steps: usize,

    /// The random seed to be used for the generation.
    #[arg(long, default_value_t = 32)]
    seed: i64,

    /// The number of samples to generate.
    #[arg(long, default_value_t = 1)]
    num_samples: i64,

    /// The name of the final image to generate.
    #[arg(long, value_name = "FILE", default_value = "sd_final.png")]
    final_image: String,

    /// Do not use autocast.
    #[arg(long, action)]
    no_autocast: bool,
}

fn prepare_mask_and_masked_image<T: AsRef<std::path::Path>>(
    path_input: T,
    path_mask: T,
) -> anyhow::Result<(Tensor, Tensor)> {
    let image = tch::vision::image::load(path_input)?;
    let image = image / 255. * 2. - 1.;

    let mask = tch::vision::image::load(path_mask)?;
    let mask = mask.mean_dim(Some([1].as_slice()), true, Kind::Float);
    let mask = mask.ge(122.5).totype(Kind::Float);
    let masked_image: Tensor = image * (1 - &mask);
    Ok((mask.unsqueeze(0), masked_image.unsqueeze(0)))
}

fn run(args: Args) -> anyhow::Result<()> {
    let Args {
        prompt,
        cpu,
        n_steps,
        seed,
        final_image,
        vae_weights,
        clip_weights,
        unet_weights,
        sliced_attention_size,
        num_samples,
        input_image,
        mask_image,
        no_autocast: _,
    } = args;
    tch::maybe_init_cuda();
    println!("Cuda available: {}", tch::Cuda::is_available());
    println!("Cudnn available: {}", tch::Cuda::cudnn_is_available());
    let cuda_device = Device::cuda_if_available();
    let cpu_or_cuda = |name: &str| {
        if cpu.iter().any(|c| c == "all" || c == name) {
            Device::Cpu
        } else {
            cuda_device
        }
    };
    let (mask, masked_image) = prepare_mask_and_masked_image(input_image, mask_image)?;
    let clip_device = cpu_or_cuda("clip");
    let vae_device = cpu_or_cuda("vae");
    let unet_device = cpu_or_cuda("unet");
    let scheduler = ddim::DDIMScheduler::new(n_steps, 1000, Default::default());

    let tokenizer = clip::Tokenizer::create("data/bpe_simple_vocab_16e6.txt")?;
    let prompt = prompt.unwrap_or_else(|| {
        "A very realistic photo of a rusty robot walking on a sandy beach".to_string()
    });
    println!("Running with prompt \"{prompt}\".");
    let tokens = tokenizer.encode(&prompt)?;
    let tokens: Vec<i64> = tokens.into_iter().map(|x| x as i64).collect();
    let tokens = Tensor::of_slice(&tokens).view((1, -1)).to(clip_device);
    let uncond_tokens = tokenizer.encode("")?;
    let uncond_tokens: Vec<i64> = uncond_tokens.into_iter().map(|x| x as i64).collect();
    let uncond_tokens = Tensor::of_slice(&uncond_tokens).view((1, -1)).to(clip_device);

    let no_grad_guard = tch::no_grad_guard();

    println!("Building the Clip transformer.");
    let text_model = stable_diffusion::build_clip_transformer(&clip_weights, clip_device)?;
    let text_embeddings = text_model.forward(&tokens);
    let uncond_embeddings = text_model.forward(&uncond_tokens);
    let text_embeddings = Tensor::cat(&[uncond_embeddings, text_embeddings], 0).to(unet_device);

    println!("Building the autoencoder.");
    let vae = stable_diffusion::build_vae(&vae_weights, vae_device)?;
    println!("Building the unet.");
    let unet = stable_diffusion::build_unet(&unet_weights, unet_device, sliced_attention_size)?;

    // torch.nn.functional.interpolate(mask, size=(height // 8, width // 8))
    let mask = mask.upsample_nearest2d(&[HEIGHT / 8, WIDTH / 8], None, None);
    let mask = Tensor::cat(&[&mask, &mask], 0).to_device(unet_device);
    let masked_image_dist = vae.encode(&masked_image.to_device(vae_device));

    let bsize = 1;
    for idx in 0..num_samples {
        tch::manual_seed(seed + idx);
        let masked_image_latents = (masked_image_dist.sample() * 0.18215).to(unet_device);
        let masked_image_latents = Tensor::cat(&[&masked_image_latents, &masked_image_latents], 0);
        let mut latents =
            Tensor::randn(&[bsize, 4, HEIGHT / 8, WIDTH / 8], (Kind::Float, unet_device));

        for (timestep_index, &timestep) in scheduler.timesteps().iter().enumerate() {
            println!("Timestep {timestep_index}/{n_steps}");
            let latent_model_input = Tensor::cat(&[&latents, &latents], 0);
            let latent_model_input =
                Tensor::cat(&[&latent_model_input, &mask, &masked_image_latents], 1);
            let noise_pred = unet.forward(&latent_model_input, timestep as f64, &text_embeddings);
            let noise_pred = noise_pred.chunk(2, 0);
            let (noise_pred_uncond, noise_pred_text) = (&noise_pred[0], &noise_pred[1]);
            let noise_pred =
                noise_pred_uncond + (noise_pred_text - noise_pred_uncond) * GUIDANCE_SCALE;
            latents = scheduler.step(&noise_pred, timestep, &latents);
        }

        println!("Generating the final image for sample {}/{}.", idx + 1, num_samples);
        let latents = latents.to(vae_device);
        let image = vae.decode(&(&latents / 0.18215));
        let image = (image / 2 + 0.5).clamp(0., 1.).to_device(Device::Cpu);
        let image = (image * 255.).to_kind(Kind::Uint8);
        let final_image = if num_samples > 1 {
            match final_image.rsplit_once('.') {
                None => format!("{}.{}.png", final_image, idx + 1),
                Some((filename_no_extension, extension)) => {
                    format!("{}.{}.{}", filename_no_extension, idx + 1, extension)
                }
            }
        } else {
            final_image.clone()
        };
        tch::vision::image::save(&image, &final_image)?;
    }

    drop(no_grad_guard);
    Ok(())
}

fn main() -> anyhow::Result<()> {
    let args = Args::parse();
    if args.no_autocast {
        run(args)
    } else {
        tch::autocast(true, || run(args))
    }
}