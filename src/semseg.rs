use crate::caps;
use crate::cata;
use crate::registry;
use gst;
use gst_video;
use opencv::core;
use std::i32;
use std::mem::transmute;
use std::sync::Mutex;
use tch;
use tch::Tensor;

const WIDTH: i32 = 640;
const HEIGHT: i32 = 192;

fn label_map() -> Tensor {
        //         name                     id    trainId   category            catId     hasInstances   ignoreInEval   color
        // Label(  'road'                 ,  0 ,        0 , 'flat'            , 1       , False        , False        , (128,  64, 128) ),
        // Label(  'sidewalk'             ,  1 ,        1 , 'flat'            , 1       , False        , False        , (244,  35, 232) ),
        // Label(  'building'             ,  2 ,        2 , 'construction'    , 2       , False        , False        , ( 70,  70,  70) ),
        // Label(  'wall'                 ,  3 ,        3 , 'construction'    , 2       , False        , False        , (102, 102, 156) ),
        // Label(  'fence'                ,  4 ,        4 , 'construction'    , 2       , False        , False        , (190, 153, 153) ),
        // Label(  'pole'                 ,  5 ,        5 , 'object'          , 3       , False        , False        , (153, 153, 153) ),
        // Label(  'traffic light'        ,  6 ,        6 , 'object'          , 3       , False        , False        , (250, 170,  30) ),
        // Label(  'traffic sign'         ,  7 ,        7 , 'object'          , 3       , False        , False        , (220, 220,   0) ),
        // Label(  'vegetation'           ,  8 ,        8 , 'nature'          , 4       , False        , False        , (107, 142,  35) ),
        // Label(  'terrain'              ,  9 ,        9 , 'nature'          , 4       , False        , False        , (152, 251, 152) ),
        // Label(  'sky'                  , 10 ,       10 , 'sky'             , 5       , False        , False        , ( 70, 130, 180) ),
        // Label(  'person'               , 11 ,       11 , 'human'           , 6       , True         , False        , (220,  20,  60) ),
        // Label(  'rider'                , 12 ,       12 , 'human'           , 6       , True         , False        , (255,   0,   0) ),
        // Label(  'car'                  , 13 ,       13 , 'vehicle'         , 7       , True         , False        , (  0,   0, 142) ),
        // Label(  'truck'                , 14 ,       14 , 'vehicle'         , 7       , True         , False        , (  0,   0,  70) ),
        // Label(  'bus'                  , 15 ,       15 , 'vehicle'         , 7       , True         , False        , (  0,  60, 100) ),
        // Label(  'train'                , 16 ,       16 , 'vehicle'         , 7       , True         , False        , (  0,  80, 100) ),
        // Label(  'motorcycle'           , 17 ,       17 , 'vehicle'         , 7       , True         , False        , (  0,   0, 230) ),
        // Label(  'bicycle'              , 18 ,       18 , 'vehicle'         , 7       , True         , False        , (119,  11,  32) ),

    let mut labels = vec![vec![30, 15, 60]; 19];
    labels[ 0] = vec![128,  64, 128]; // road
    labels[ 1] = vec![244,  35, 232]; // sidewalk
    labels[ 2] = vec![ 70,  70,  70]; // building
    labels[11] = vec![220,  20,  60]; // person
    labels[12] = vec![255,   0,   0]; // rider
    labels[13] = vec![  0,   0, 142]; // car
    labels[14] = vec![  0,   0,  70]; // truck
    labels[15] = vec![  0,  60, 100]; // bus
    labels[16] = vec![  0,  80, 100]; // train
    labels[17] = vec![  0,   0, 230]; // motorcycle
    labels[18] = vec![119,  11,  32]; // bicycle
    let labels = labels.into_iter().flatten().collect::<Vec<u8>>();
    Tensor::of_slice(&labels)
    .reshape(&[19, 1, 3])
    .permute(&[2, 1, 0])
}

lazy_static! {
    static ref CAPS: Mutex<gst::Caps> = Mutex::new(gst::Caps::new_simple(
        "video/x-raw",
        &[
            (
                "format",
                &gst::List::new(&[&gst_video::VideoFormat::Rgb.to_str()]),
            ),
            ("width", &WIDTH),
            ("height", &HEIGHT),
            (
                "framerate",
                &gst::FractionRange::new(gst::Fraction::new(0, 1), gst::Fraction::new(i32::MAX, 1),),
            ),
        ],
    ));
    static ref SEMSEG_MODEL: Mutex<tch::CModule> =
        Mutex::new(tch::CModule::load("models/semseg/semseg.pt").unwrap());
}

pub struct SemSeg {
    video_info: gst_video::VideoInfo,
    color_map: Tensor, // Tensor[[3, 1, 728], Uint8]
}

impl registry::Registry for SemSeg {
    const NAME: &'static str = "semseg";
    const DEBUG_CATEGORY: &'static str = "semseg";
    register_typedata!();
}

impl std::default::Default for SemSeg {
    fn default() -> Self {
        let caps = gst::Caps::fixate(CAPS.lock().unwrap().clone());
        SemSeg {
            video_info: gst_video::VideoInfo::from_caps(&caps).unwrap(),
            color_map: label_map().to_device(tch::Device::Cuda(0)),
        }
    }
}

impl caps::CapsDef for SemSeg {
    fn caps_def() -> (Vec<caps::PadCaps>, Vec<caps::PadCaps>) {
        let in_caps = caps::PadCaps {
            name: "rgb",
            caps: CAPS.lock().unwrap().clone(),
        };
        let out_caps = caps::PadCaps {
            name: "depth",
            caps: CAPS.lock().unwrap().clone(),
        };
        (vec![in_caps], vec![out_caps])
    }
}

impl cata::Process for SemSeg {
    fn process(
        &mut self,
        inbuf: &Vec<gst::Buffer>,
        outbuf: &mut Vec<gst::Buffer>,
    ) -> Result<(), std::io::Error> {
        for (i, buf) in inbuf.iter().enumerate() {
            if i < outbuf.len() {
                outbuf[i] = buf.clone();
            }
        }

        let mut depth_buf = inbuf[0].copy_deep().unwrap();
        {
            let rgb_ref = inbuf[0].as_ref();
            let in_frame =
                gst_video::VideoFrameRef::from_buffer_ref_readable(rgb_ref, &self.video_info)
                    .unwrap();
            let _in_stride = in_frame.plane_stride()[0] as usize;
            let _in_format = in_frame.format();
            let in_width = in_frame.width() as i32;
            let in_height = in_frame.height() as i32;
            let in_data = in_frame.plane_data(0).unwrap();
            let in_mat = core::Mat::new_rows_cols_with_data(
                in_height,
                in_width,
                core::CV_8UC3,
                unsafe { transmute(in_data.as_ptr()) },
                0,
            )
            .unwrap();

            let depth_ref = depth_buf.get_mut().unwrap();
            let mut out_frame =
                gst_video::VideoFrameRef::from_buffer_ref_writable(depth_ref, &self.video_info)
                    .unwrap();
            let _out_stride = out_frame.plane_stride()[0] as usize;
            let _out_format = out_frame.format();
            let out_data = out_frame.plane_data_mut(0).unwrap();

            let img_slice = unsafe {
                std::slice::from_raw_parts(in_mat.data().unwrap(), (WIDTH * HEIGHT * 3) as usize)
            };
            let img = tch::Tensor::of_slice(img_slice)
                .to_kind(tch::Kind::Uint8)
                .reshape(&[HEIGHT as i64, WIDTH as i64, 3])
                .permute(&[2, 0, 1])
                .to_device(tch::Device::Cuda(0));
            let img = img.to_kind(tch::Kind::Float) / 255;
            let img: tch::IValue = tch::IValue::Tensor(img.unsqueeze(0));

            let semseg_pred = SEMSEG_MODEL.lock().unwrap().forward_is(&[img]).unwrap();
            let semseg_pred = if let tch::IValue::Tensor(semseg_pred) = &semseg_pred {
                    Some(semseg_pred)
                } else { None };
            // println!("color_map: {:?}", self.color_map);
            // println!("semseg_pred: {:?}", semseg_pred);
            let semseg_pred = semseg_pred.unwrap().squeeze(); 
            let semseg_pred = semseg_pred.argmax(0, false).to_kind(tch::Kind::Uint8);

            let color_index = semseg_pred
                .flatten(0, 1)
                .to_kind(tch::Kind::Int64);

            let semseg_color = self
                .color_map
                .index_select(2, &color_index)
                .permute(&[2, 1, 0])
                .to_device(tch::Device::Cpu);

            let semseg_color = Vec::<u8>::from(semseg_color);
            unsafe {
                std::ptr::copy_nonoverlapping(
                    semseg_color.as_ptr(),
                    out_data.as_mut_ptr(),
                    (HEIGHT * WIDTH * 3) as usize,
                );
            }
        }

        outbuf[0] = depth_buf;

        Ok(())
    }
}
