#!/bin/sh

ROOT=$(pwd)

wget_vocab() {
   wget https://github.com/openai/CLIP/raw/main/clip/bpe_simple_vocab_16e6.txt.gz
   gunzip bpe_simple_vocab_16e6.txt.gz
}

wget_clip_weights() {
  wget -c https://huggingface.co/openai/clip-vit-large-patch14/resolve/main/pytorch_model.bin
  python3 -c "
import numpy as np
import torch

model = torch.load('./pytorch_model.bin')
np.savez('./pytorch_model.npz', **{k: v.numpy() for k, v in model.items() if 'text_model' in k})
"
}

wget_vae_unet_weights() {
  # download weights for vae
  header="Authorization: Bearer $1"
  wget --header="$header" https://huggingface.co/CompVis/stable-diffusion-v1-4/resolve/main/vae/diffusion_pytorch_model.bin -O vae.bin
  # download weights for unet
  wget --header="$header" https://huggingface.co/CompVis/stable-diffusion-v1-4/resolve/main/unet/diffusion_pytorch_model.bin -O unet.bin	
  
  # convert to npz
  python3 -c "
import numpy as np
import torch
  
model = torch.load('./vae.bin')
np.savez('./vae.npz', **{k: v.numpy() for k, v in model.items()})
  
model = torch.load('./unet.bin')
np.savez('./unet.npz', **{k: v.numpy() for k, v in model.items()})
"
}

tch_tools_to_ot() {
  cd $ROOT/tch-rs
  cargo run --release --example tensor-tools cp ../data/$1.npz ../data/$1.ot
}


if [ $# -ne 1 ]; then
    echo 'Usage: ./download_weights.sh <HUGGINGFACE_TOKEN>' >&2
    exit 1
fi

echo "Setting up for diffusers-rs..."

git clone https://github.com/LaurentMazare/tch-rs
mkdir -p data
cd data

echo "Getting the Weights and the Vocab File"
# get the weights
wget_vocab 
wget_clip_weights 
wget_vae_unet_weights $1 

echo "Converting the Weights..."
# convert the weights 
tch_tools_to_ot pytorch_model 
tch_tools_to_ot vae 
tch_tools_to_ot unet 

echo "Cleaning ..."
rm -rf $ROOT/tch-rs
rm -rf $ROOT/data/*.npz $ROOT/data/*.bin

echo "Done."
